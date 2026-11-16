#![forbid(unsafe_code)]

use std::{
    cell::LazyCell,
    hint::black_box,
    num::{NonZeroU32, NonZeroUsize},
    task::{Context, Waker},
};

use criterion::{BenchmarkId, Criterion, SamplingMode, criterion_group, criterion_main};
use firewheel::{
    StreamInfo,
    clock::InstantSamples,
    dsp::{buffer::ConstSequentialBuffer, declick::DeclickValues},
    log::{RealtimeLoggerConfig, realtime_logger},
    mask::{ConnectedMask, ConstantMask, SilenceMask},
    node::{
        AudioNode, AudioNodeProcessor, ConstructProcessorContext, EmptyConfig, NUM_SCRATCH_BUFFERS,
        NodeID, ProcBuffers, ProcExtra, ProcInfo, ProcStore, StreamStatus,
    },
};
use kithara::{
    audio::{AudioConfig, mock::TestPcmReader},
    effects::{
        LimiterConfig, LookaheadMs,
        eq::{EqConfig, EqLayout, generate_log_spaced_bands},
        node::LimiterNode,
    },
    file::{File, FileConfig, FileSrc},
    platform::{
        sync::Arc,
        time::{Duration, Instant},
    },
    play::{PlayWorker, PlayWorkerConfig},
    queue::TrackSource,
    signal::{AudioSpec, OutputContext, SegmentId, SessionEpoch, SessionFrame, TransportRevision},
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
        CrossfaderGain, CurveShape, DeckEnds, DeckEqChange, DeckMixSettingsChange, DeckPart,
        DeckProtocol, EqGainProgram, Fade, FilterProgram, ParameterKnot, SessionInbox, Slot,
        SlotState, scope_channels,
    },
    rt::{
        DeckMixer, DeckMixerConfig, StreamShape, install_render_context, publish_render_context,
        track::{PcmConsumer, PlayerResource},
    },
};
use kithara_test_fixtures::integration_fixtures::benchmark_half;
use kithara_test_utils::wait_until_blocking;

mod consts {
    pub(super) const BLOCK_FRAMES: u32 = 128;
    pub(super) const CHANNELS: u16 = 2;
    /// Longer than each warm-up and measured window, at most 22 000 blocks
    /// of 128 frames at 48 kHz (about 58.7 s), so every measured block of
    /// the crossfade lane mixes through the fade law.
    pub(super) const CROSSFADE_SECONDS: f32 = 120.0;
    pub(super) const CROSSFADE_DEPTH: f32 = 0.7;
    pub(super) const CROSSFADE_POSITION: f32 = 0.3;
    /// The highest rate a Host accepts. Its master limiter reserves the
    /// lookahead line for this rate, so the bench limiter does as well.
    pub(super) const HOST_RATE_BOUND: u32 = 768_000;
    /// Each lane and track count is measured at these lookaheads in turn: none,
    /// which builds no delay line, and 5 ms, 240 frames at 48 kHz.
    pub(super) const LOOKAHEAD_MS: [f32; 2] = [0.0, 5.0];
    pub(super) const MEASURED_BLOCKS: usize = 20_000;
    pub(super) const SAMPLE_RATE: u32 = 48_000;
    pub(super) const TRACK_COUNTS: [usize; 3] = [1, 2, 4];
    pub(super) const TRACK_SECONDS: f64 = 600.0;
    pub(super) const WARMUP_BLOCKS: usize = 2_000;
    /// EQ bands of the automation lanes, the deck program capacity.
    pub(super) const EQ_BANDS: usize = 16;
    /// Blocks rendered between preparing the automated stages and their programs.
    pub(super) const SETTLE_BLOCKS: usize = 4;
}

/// What the tracks of a lane mix through on every measured block.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Lane {
    /// A declick that settles before the warm-up ends: a unity mix.
    Settled,
    /// A settled output gain of 0.5.
    Gain,
    /// One equal-power crossfade that spans every warm-up and measured block.
    Crossfade,
    /// An EQ layout, with `filter` deck filter knob programs and `eq` band
    /// gain programs moving on every warm-up and measured frame.
    Automation { filter: usize, eq: usize },
}

impl Lane {
    fn fade(self) -> Fade {
        match self {
            Self::Settled | Self::Gain | Self::Automation { .. } => Fade::Declick,
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

    fn label(self) -> String {
        match self {
            Self::Settled => "settled".to_owned(),
            Self::Gain => "settled_gain".to_owned(),
            Self::Crossfade => "active_crossfade".to_owned(),
            Self::Automation { filter, eq } => format!("filter_{filter}_eq_{eq}"),
        }
    }
}

fn non_zero(value: u32, label: &str) -> NonZeroU32 {
    NonZeroU32::new(value).unwrap_or_else(|| panic!("bench {label} must be non-zero"))
}

fn block_frames() -> usize {
    usize::try_from(consts::BLOCK_FRAMES)
        .unwrap_or_else(|_| panic!("bench block frames exceed usize"))
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
    limiter: Box<dyn AudioNodeProcessor>,
    limited: [Vec<f32>; 2],
    extra: ProcExtra,
    control: ScopedSender<DeckProtocol, DeckProtocol>,
    deck: DeckEnds,
    lanes: Vec<Box<dyn LaneTask + Send>>,
    _lane_senders: Vec<Sender<LaneProtocol>>,
    frame: i64,
}

#[kithara_test_utils::kithara::hang_watchdog]
async fn processor(sources: &[PcmDeck], lane: Lane, limiter: LimiterConfig) -> BenchMixer {
    let config = match lane {
        Lane::Automation { .. } => DeckMixerConfig::builder()
            .eq_bands(consts::EQ_BANDS)
            .build(),
        _ => DeckMixerConfig::default(),
    };
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
    let mut lanes = Vec::new();
    let mut senders = Vec::new();
    let mut commands = Vec::new();
    if lane == Lane::Gain {
        commands.push(DeckPart::Mix(DeckMixSettingsChange::Crossfader(
            CrossfaderGain::checked(0.5).expect("half is a crossfader gain"),
        )));
    }
    for (index, source) in sources.iter().enumerate() {
        hang_tick!();
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
        let start = LaneStart::default();
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
        lanes.push(Box::new(task) as Box<dyn LaneTask + Send>);
        senders.push(sender);
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
    let stream = StreamInfo {
        sample_rate: shape.sample_rate,
        sample_rate_recip: f64::from(shape.sample_rate.get()).recip(),
        prev_sample_rate: shape.sample_rate,
        max_block_frames: shape.max_block_frames,
        ..StreamInfo::default()
    };
    let mut state = None;
    let limiter = LimiterNode::new(
        limiter,
        non_zero(consts::HOST_RATE_BOUND, "Host rate bound"),
        pools,
    )
    .construct_processor(
        &EmptyConfig,
        ConstructProcessorContext::new(NodeID::DANGLING, &stream, &mut state),
    )
    .expect("bench master limiter");
    BenchMixer {
        processor,
        limiter: Box::new(limiter),
        limited: std::array::from_fn(|_| vec![0.0; block_frames()]),
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
        frame: 0,
    }
}

fn prepare_sources(mixer: &mut BenchMixer) {
    if mixer.frame == 0 {
        for lane in &mut mixer.lanes {
            lane.warm_up();
        }
    }
    let required = f64::from(consts::BLOCK_FRAMES) / f64::from(consts::SAMPLE_RATE);
    let _real_io = kithara::platform::flash::real_io();
    wait_until_blocking(Duration::from_secs(5), "bench source not ready", || {
        let mut context = Context::from_waker(Waker::noop());
        for lane in &mut mixer.lanes {
            let _ = lane.poll_commands(&mut context);
            lane.recycle();
            let _ = lane.tick();
        }
        if mixer.frame == 0 {
            mixer.lanes.iter_mut().all(|lane| {
                lane.preload_status()
                    .unwrap_or_else(|error| panic!("source not ready: {error:?}"))
            })
        } else {
            (0..mixer.lanes.len()).all(|index| {
                let slot = Slot::new(u16::try_from(index).expect("bench slot"));
                mixer.processor.track(slot).is_some_and(|track| {
                    track.state() == SlotState::Playing
                        && track.decoded_frontier() - track.position() >= required
                })
            })
        }
    })
    .unwrap_or_else(|error| {
        let sources: Vec<_> = (0..mixer.lanes.len())
            .map(|index| {
                let slot = Slot::new(u16::try_from(index).expect("bench slot"));
                mixer
                    .processor
                    .track(slot)
                    .map(|track| (track.position(), track.decoded_frontier()))
            })
            .collect();
        panic!(
            "source not ready for {} frames: {error}; (position, frontier)={sources:?}",
            consts::BLOCK_FRAMES
        );
    });
}

fn render_block(mixer: &mut BenchMixer, out_l: &mut [f32], out_r: &mut [f32]) -> Duration {
    prepare_sources(mixer);
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
        OutputContext::new(
            SessionFrame::new(mixer.frame)..SessionFrame::new(end),
            non_zero(consts::SAMPLE_RATE, "sample rate"),
            SessionEpoch::new(0),
            Some(TransportRevision::first()),
        )
        .expect("bench output range"),
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
    let mut outputs = [&mut *out_l, &mut *out_r];
    let buffers = ProcBuffers {
        inputs: &inputs,
        outputs: &mut outputs,
    };
    let start = Instant::now();
    let outcome = mixer.processor.process(&info, buffers, &mut mixer.extra);
    let inputs = [&*out_l, &*out_r];
    let mut outputs = mixer.limited.each_mut().map(Vec::as_mut_slice);
    let limited = mixer.limiter.process(
        &info,
        ProcBuffers {
            inputs: &inputs,
            outputs: &mut outputs,
        },
        &mut mixer.extra,
    );
    let elapsed = start.elapsed();
    black_box((outcome, limited));
    out_l.copy_from_slice(&mixer.limited[0]);
    out_r.copy_from_slice(&mixer.limited[1]);
    mixer.frame = end;
    while mixer.control.receipt().is_some() {}
    elapsed
}

fn send_parts(mixer: &mut BenchMixer, at: When<SessionFrame>, commands: Vec<DeckPart>) {
    let scope = mixer.deck.scope;
    mixer
        .control
        .scope(scope)
        .expect("bench scope")
        .send(
            at,
            Batch {
                basis: Vec::new(),
                commands,
            },
        )
        .expect("bench deck channel capacity");
    mixer.control.publish().expect("publish bench commands");
}

fn linear(start: i64, end: i64, from: f64, to: f64) -> Vec<ParameterKnot> {
    vec![
        ParameterKnot::new(SessionFrame::new(start), from, CurveShape::LINEAR)
            .expect("bench first knot"),
        ParameterKnot::new(SessionFrame::new(end), to, CurveShape::HOLD).expect("bench last knot"),
    ]
}

/// Installs an EQ layout, then programs that keep moving through every
/// warm-up and measured frame. The knob program stays in the low-pass half,
/// so the deck filter runs on every frame.
fn automate(
    mixer: &mut BenchMixer,
    filter: usize,
    eq: usize,
    out_l: &mut [f32],
    out_r: &mut [f32],
) {
    for _ in 0..consts::SETTLE_BLOCKS {
        render_block(mixer, out_l, out_r);
    }
    let rate = non_zero(consts::SAMPLE_RATE, "sample rate");
    let layout = EqLayout::new(
        &EqConfig::builder(pools()).build(),
        &generate_log_spaced_bands(consts::EQ_BANDS),
        rate,
    )
    .expect("bench EQ layout");
    send_parts(
        mixer,
        When::Next,
        vec![DeckPart::Eq(DeckEqChange::Layout(Box::new(layout)))],
    );
    for _ in 0..consts::SETTLE_BLOCKS {
        render_block(mixer, out_l, out_r);
    }
    let block = i64::from(consts::BLOCK_FRAMES);
    let start = mixer.frame + 2 * block;
    let blocks = consts::WARMUP_BLOCKS + consts::MEASURED_BLOCKS + consts::SETTLE_BLOCKS;
    let end = start + block * i64::try_from(blocks).expect("bench blocks fit i64");
    let snapshot = mixer.deck.snapshot.read();
    let filter_binding = snapshot.filter.binding().expect("observed output stream");
    let eq_binding = snapshot.eq.binding().expect("installed EQ layout");
    let mut parts: Vec<DeckPart> = (0..filter)
        .map(|_| DeckPart::FilterProgram {
            binding: filter_binding,
            authority: snapshot.filter.authority(),
            program: FilterProgram::new(linear(start, end, -1.0, -0.1))
                .expect("bench filter program"),
        })
        .collect();
    for band in 0..eq {
        parts.push(DeckPart::EqProgram {
            binding: eq_binding,
            authority: snapshot.eq.authority(band),
            program: EqGainProgram::new(band, linear(start, end, -24.0, 6.0))
                .expect("bench EQ program"),
        });
    }
    // A program's batch is due on its first knot.
    send_parts(mixer, When::At(SessionFrame::new(start)), parts);
    for _ in 0..consts::SETTLE_BLOCKS {
        render_block(mixer, out_l, out_r);
    }
    let snapshot = mixer.deck.snapshot.read();
    let running = usize::from(snapshot.filter.program().is_some())
        + (0..consts::EQ_BANDS)
            .filter(|band| snapshot.eq.program(*band).is_some())
            .count();
    assert_eq!(running, filter + eq, "every bench program must be admitted");
}

fn peak_of(samples: &[f32]) -> f32 {
    samples.iter().fold(0.0_f32, |acc, s| acc.max(s.abs()))
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

fn measure(sources: &[PcmDeck], lane: Lane, limiter: LimiterConfig, blocks: usize) -> Duration {
    assert!((1..=consts::MEASURED_BLOCKS).contains(&blocks));
    let runtime = tokio::runtime::Runtime::new().expect("bench runtime");
    let mut processor = runtime.block_on(processor(sources, lane, limiter));
    let _runtime = runtime.enter();

    let frames = block_frames();
    let mut out_l = vec![0.0_f32; frames];
    let mut out_r = vec![0.0_f32; frames];
    if let Lane::Automation { filter, eq } = lane {
        automate(&mut processor, filter, eq, &mut out_l, &mut out_r);
    }

    for _ in 0..consts::WARMUP_BLOCKS {
        render_block(&mut processor, &mut out_l, &mut out_r);
    }

    let fading_before = every_slot_fading(&mut processor);
    let before = processor.deck.snapshot.read().metrics;
    let mut elapsed = Duration::ZERO;
    let mut peak = 0.0_f32;
    for _ in 0..blocks {
        elapsed += render_block(&mut processor, &mut out_l, &mut out_r);
        peak = peak.max(peak_of(&out_l));
        black_box(&out_l);
    }
    let after = processor.deck.snapshot.read().metrics;
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
    let tracks = sources.len();
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
    assert!(
        peak <= f32::from(LimiterConfig::default().ceiling()),
        "master limiter must bound the audible mix"
    );

    black_box(peak);
    elapsed
}

fn bench_blocks(c: &mut Criterion) {
    let mut group = c.benchmark_group("rt_block_budget");
    group.sampling_mode(SamplingMode::Flat);
    group.sample_size(20);
    group.warm_up_time(Duration::from_secs(1));
    group.measurement_time(Duration::from_secs(4));
    let automation = [(0, 0), (1, 0), (0, 16), (1, 15)]
        .map(|(filter, eq)| Lane::Automation { filter, eq })
        .into_iter()
        .flat_map(|lane| consts::LOOKAHEAD_MS.map(|ms| ((lane, 1), ms)));
    for ((lane, count), lookahead) in [Lane::Settled, Lane::Gain, Lane::Crossfade]
        .into_iter()
        .flat_map(|lane| consts::TRACK_COUNTS.map(|count| (lane, count)))
        .flat_map(|row| consts::LOOKAHEAD_MS.map(|ms| (row, ms)))
        .chain(automation)
    {
        let limiter = LimiterConfig::builder()
            .lookahead_ms(LookaheadMs::try_from(lookahead).expect("bench lookahead in range"))
            .build();
        let latency = limiter.latency_frames(non_zero(consts::SAMPLE_RATE, "sample rate"));
        let sources = LazyCell::new(|| {
            (0..count)
                .map(|_| {
                    PcmDeck::new(Box::new(TestPcmReader::with_pcm(
                        spec(),
                        consts::TRACK_SECONDS,
                        benchmark_half(),
                    )))
                })
                .collect::<Vec<_>>()
        });
        group.bench_function(
            BenchmarkId::new(lane.label(), format!("{count}tracks_L{latency}")),
            |b| {
                b.iter_custom(|mut blocks| {
                    let mut elapsed = Duration::ZERO;
                    let limit =
                        u64::try_from(consts::MEASURED_BLOCKS).expect("window blocks fit u64");
                    // Reset before the crossfade or automation can finish.
                    while blocks > 0 {
                        let batch = blocks.min(limit);
                        elapsed += measure(
                            &sources,
                            lane,
                            limiter,
                            usize::try_from(batch).expect("bounded window fits usize"),
                        );
                        blocks -= batch;
                    }
                    elapsed
                });
            },
        );
    }
    group.finish();
}

criterion_group!(benches, bench_blocks);
criterion_main!(benches);
