#![forbid(unsafe_code)]

use std::{
    fs,
    hint::black_box,
    num::{NonZeroU32, NonZeroU64},
    path::Path,
};

use criterion::{
    BatchSize, BenchmarkId, Criterion, SamplingMode, Throughput, criterion_group, criterion_main,
};
use kithara::{
    analysis::{
        AnalysisDemand, AnalysisFile, AnalysisFileSpec, AnalysisProgress, AnalysisToken,
        AnalysisWorker, AnalysisWorkerConfig, AnalyzerBuilder, BeatAnalysisConfig,
    },
    assets::{AssetStore, StorageBackend},
    audio::{AudioConfig, AudioRead, ReadOutcome},
    file::{File, FileConfig},
    platform::{
        time::Duration,
        tokio::runtime::{Builder, Runtime},
    },
    play::{PlayWorker, PlayWorkerConfig, PlaybackResamplerBackend},
    signal::{AudioChunk, AudioChunkInfo, AudioSpec},
    stretch::{ElasticConfig, ElasticEngine, ElasticRequest, StretchKind, build_engine},
    warp::{Warp, WarpConfig, WarpRenderer},
};
use kithara_integration_tests::bufpool_ext::{Pools, TestPools, pools};
use kithara_test_fixtures::assets::signal_mp3_track_sine440_187s;
use num_traits::ToPrimitive;
use tempfile::TempDir;

mod limiter {
    use std::{
        hint::black_box,
        num::{NonZeroU32, NonZeroUsize},
    };

    use audioadapter_buffers::direct::InterleavedSlice;
    use criterion::{BatchSize, BenchmarkGroup, BenchmarkId, Criterion, measurement::WallTime};
    use firewheel::{
        ActivateInfo, FirewheelConfig, FirewheelContext, StreamInfo,
        backend::BackendProcessInfo,
        channel_config::ChannelCount,
        clock::InstantSamples,
        dsp::{buffer::ConstSequentialBuffer, declick::DeclickValues},
        log::{RealtimeLoggerConfig, realtime_logger},
        mask::{ConnectedMask, ConstantMask, SilenceMask},
        node::{
            AudioNode, AudioNodeProcessor, ConstructProcessorContext, EmptyConfig,
            NUM_SCRATCH_BUFFERS, NodeID, ProcBuffers, ProcExtra, ProcInfo, ProcStore, StreamStatus,
        },
        nodes::volume::VolumeNode,
    };
    use kithara::{
        effects::{LimiterConfig, PeakLimiter, node::LimiterNode},
        platform::time::Duration,
    };

    #[derive(Clone)]
    struct Block {
        planes: [Vec<f32>; 2],
        silence: SilenceMask,
    }

    fn blocks(frames: usize) -> [Block; 4] {
        [4.0, 0.0, 0.0, 0.25].map(|value| Block {
            planes: [vec![value; frames], vec![value * 0.5; frames]],
            silence: if value == 0.0 {
                SilenceMask::new_all_silent(2)
            } else {
                SilenceMask::default()
            },
        })
    }

    fn owner(group: &mut BenchmarkGroup<'_, WallTime>, frames: usize, input: &[Block; 4]) {
        let mut limiter = PeakLimiter::new(
            NonZeroU32::new(48_000).expect("sample rate"),
            NonZeroUsize::new(2).expect("stereo"),
            LimiterConfig::default(),
        )
        .expect("limiter config");
        group.bench_function(BenchmarkId::new("owner", frames), |b| {
            b.iter_batched_ref(
                || (*input).clone(),
                |blocks| {
                    for block in blocks {
                        let [left, right] = &mut block.planes;
                        limiter.process_planar(&mut [left, right]);
                        black_box(&block.planes);
                    }
                },
                BatchSize::SmallInput,
            );
        });
    }

    fn info(frames: usize, rate: NonZeroU32) -> ProcInfo {
        ProcInfo {
            sample_rate: rate,
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
            last_marker_instant: InstantSamples(0),
            sample_rate_recip: f64::from(rate.get()).recip(),
            clock_samples: InstantSamples(0),
            duration_since_stream_start: Duration::ZERO,
            stream_status: StreamStatus::empty(),
            dropped_frames: 0,
        }
    }

    fn callback(group: &mut BenchmarkGroup<'_, WallTime>, frames: usize, input: &[Block; 4]) {
        let rate = NonZeroU32::new(48_000).expect("sample rate");
        let stream = StreamInfo {
            sample_rate: rate,
            sample_rate_recip: f64::from(rate.get()).recip(),
            prev_sample_rate: rate,
            max_block_frames: NonZeroU32::new(u32::try_from(frames).expect("block frames"))
                .expect("nonempty block"),
            ..StreamInfo::default()
        };
        let node = LimiterNode::new(LimiterConfig::default());
        let mut custom_state = None;
        let mut processor = node
            .construct_processor(
                &EmptyConfig,
                ConstructProcessorContext::new(NodeID::DANGLING, &stream, &mut custom_state),
            )
            .expect("limiter processor");
        let (logger, _receiver) = realtime_logger(RealtimeLoggerConfig::default());
        let mut extra = ProcExtra {
            logger,
            store: ProcStore::with_capacity(0),
            scratch_buffers: ConstSequentialBuffer::<f32, NUM_SCRATCH_BUFFERS>::new(frames),
            declick_values: DeclickValues::new(NonZeroU32::new(16).expect("declick frames")),
        };
        let mut info = info(frames, rate);
        let mut left = vec![0.0; frames];
        let mut right = vec![0.0; frames];
        group.bench_function(BenchmarkId::new("callback", frames), |b| {
            b.iter(|| {
                for block in input {
                    info.in_silence_mask = block.silence;
                    black_box(processor.process(
                        &info,
                        ProcBuffers {
                            inputs: &[&block.planes[0], &block.planes[1]],
                            outputs: &mut [&mut left, &mut right],
                        },
                        &mut extra,
                    ));
                    black_box((&left, &right));
                }
            });
        });
    }

    fn graph(group: &mut BenchmarkGroup<'_, WallTime>) {
        const FRAMES: usize = 128;

        let mut context = FirewheelContext::new(FirewheelConfig {
            num_graph_inputs: ChannelCount::STEREO,
            ..FirewheelConfig::default()
        });
        let limiter = context
            .add_node(LimiterNode::new(LimiterConfig::default()), None)
            .expect("graph limiter");
        let volume = context
            .add_node(VolumeNode::from_linear(1.0), None)
            .expect("graph unity volume");
        for (from, to) in [
            (context.graph_in_node_id(), limiter),
            (limiter, volume),
            (volume, context.graph_out_node_id()),
        ] {
            context
                .connect(from, to, &[(0, 0), (1, 1)], false)
                .expect("graph stereo edge");
        }
        let mut processor = context
            .activate(ActivateInfo {
                sample_rate: NonZeroU32::new(48_000).expect("sample rate"),
                max_block_frames: NonZeroU32::new(u32::try_from(FRAMES).expect("block frames"))
                    .expect("nonempty block"),
                num_stream_in_channels: 2,
                num_stream_out_channels: 2,
                input_to_output_latency_seconds: 0.0,
            })
            .expect("activate graph");
        let input = blocks(FRAMES).map(|block| {
            block.planes[0]
                .iter()
                .zip(&block.planes[1])
                .flat_map(|(&left, &right)| [left, right])
                .collect::<Vec<_>>()
        });
        let mut output = [0.0; FRAMES * 2];
        let process_info = BackendProcessInfo {
            frames: FRAMES,
            process_timestamp: None,
            duration_since_stream_start: Duration::ZERO,
            input_stream_status: StreamStatus::empty(),
            output_stream_status: StreamStatus::empty(),
            dropped_frames: 0,
            process_to_playback_delay: None,
        };
        for block in &input {
            output.fill(f32::NAN);
            processor.process(
                &InterleavedSlice::new(block, 2, FRAMES).expect("graph input"),
                &mut InterleavedSlice::new_mut(&mut output, 2, FRAMES).expect("graph output"),
                process_info.clone(),
            );
            assert!(output.iter().all(|sample| sample.is_finite()));
            if block[0] == 0.0 {
                assert!(output.iter().all(|sample| *sample == 0.0));
            } else {
                assert!(output.iter().any(|sample| *sample != 0.0));
            }
        }
        group.bench_function(BenchmarkId::new("graph_unity", FRAMES), |b| {
            b.iter(|| {
                for block in &input {
                    processor.process(
                        &InterleavedSlice::new(block, 2, FRAMES).expect("graph input"),
                        &mut InterleavedSlice::new_mut(&mut output, 2, FRAMES)
                            .expect("graph output"),
                        process_info.clone(),
                    );
                    black_box(&output);
                }
            });
        });
    }

    pub(super) fn bench(c: &mut Criterion) {
        let mut group = c.benchmark_group("limiter_lifecycle");
        group.sample_size(20);
        for frames in [1, 7, 128, 1024] {
            let input = blocks(frames);
            owner(&mut group, frames, &input);
            callback(&mut group, frames, &input);
        }
        graph(&mut group);
        group.finish();
    }
}

mod consts {
    pub(super) const CHANNELS: usize = 2;
    pub(super) const SAMPLE_RATE: u32 = 44_100;
    pub(super) const ANALYSIS_BUCKETS: usize = 2_000;
    pub(super) const ANALYSIS_CHUNK_SECONDS: u64 = 16;
    pub(super) const DECODE_READ_LIMIT: usize = 16_384;
    pub(super) const STRETCH_FRAMES: usize = 8_192;
    pub(super) const CONTROL_OUTPUT_FRAMES: usize = 4_096;
    pub(super) const CONTROL_SOURCE_FRAMES: usize = 4_800;
}

struct PrimeFixture {
    discarded_output: Vec<f32>,
    request: ElasticRequest,
    source: Vec<f32>,
    source_history: Vec<f32>,
    source_lookahead: Vec<f32>,
}

fn test_mp3_bytes() -> &'static [u8] {
    signal_mp3_track_sine440_187s().bytes()
}

fn make_runtime() -> Runtime {
    Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap_or_else(|e| panic!("failed to build tokio runtime: {e}"))
}

fn make_pcm(frames: usize) -> Vec<f32> {
    let mut state = 0x5eed_1234_u32;
    let mut pcm = Vec::with_capacity(frames * consts::CHANNELS);
    let sample_rate = consts::SAMPLE_RATE
        .to_f32()
        .unwrap_or_else(|| panic!("bench sample rate exceeds f32"));
    let phase_step = 440.0 * std::f32::consts::TAU / sample_rate;
    let mut phase = 0.0_f32;
    for _ in 0..frames {
        state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        let noise = (state >> 8)
            .to_f32()
            .unwrap_or_else(|| panic!("bench noise sample exceeds f32"))
            / 16_777_215.0
            - 0.5;
        let sample = phase.sin().mul_add(0.4, noise * 0.1);
        pcm.push(sample);
        pcm.push(sample * 0.8);
        phase += phase_step;
        if phase >= std::f32::consts::TAU {
            phase -= std::f32::consts::TAU;
        }
    }
    pcm
}

fn frame_throughput(frames: usize) -> Throughput {
    Throughput::Elements(
        u64::try_from(frames).unwrap_or_else(|_| panic!("bench frame count exceeds u64")),
    )
}

fn make_chunk(pools: &Pools, pcm: &[f32]) -> AudioChunk {
    let spec = AudioSpec::new(
        u16::try_from(consts::CHANNELS).unwrap_or_else(|_| panic!("bench channels")),
        NonZeroU32::new(consts::SAMPLE_RATE).unwrap_or_else(|| panic!("bench sample rate")),
    );
    let mut samples = pools
        .get_with_len::<f32>(pcm.len())
        .unwrap_or_else(|error| panic!("bench PCM allocation failed: {error}"));
    samples.clone_from_slice(pcm);
    AudioChunk::new(
        AudioChunkInfo {
            spec,
            frames: u32::try_from(pcm.len() / consts::CHANNELS)
                .unwrap_or_else(|_| panic!("bench frame count")),
            ..AudioChunkInfo::default()
        },
        samples,
    )
}

fn stretch_config(backend: StretchKind, pools: &Pools) -> ElasticConfig<TestPools> {
    ElasticConfig::builder()
        .backend(backend)
        .pools(pools.clone())
        .sample_rate(consts::SAMPLE_RATE)
        .channels(consts::CHANNELS)
        .max_source_frames(consts::STRETCH_FRAMES)
        .max_output_frames(consts::STRETCH_FRAMES)
        .build()
        .unwrap_or_else(|error| panic!("invalid stretch benchmark config: {error}"))
}

fn stretch_engine(backend: StretchKind, pools: &Pools) -> Box<dyn ElasticEngine> {
    build_engine(stretch_config(backend, pools))
        .unwrap_or_else(|error| panic!("failed to prepare {backend} benchmark engine: {error}"))
}

fn prime_fixture(engine: &dyn ElasticEngine) -> PrimeFixture {
    let latency = engine.capabilities().latency();
    let request = ElasticRequest::new(latency.second(), latency.second())
        .unwrap_or_else(|error| panic!("invalid {latency:?} benchmark prime request: {error}"));
    PrimeFixture {
        discarded_output: vec![0.0; request.output_frames() * consts::CHANNELS],
        request,
        source: make_pcm(request.source_frames()),
        source_history: make_pcm(latency.first()),
        source_lookahead: make_pcm(latency.first()),
    }
}

fn prime_engine(engine: &mut dyn ElasticEngine, fixture: &mut PrimeFixture) {
    engine
        .prime(
            fixture.request,
            &fixture.source_history,
            &fixture.source_lookahead,
            &fixture.source,
            &mut fixture.discarded_output,
        )
        .unwrap_or_else(|error| panic!("failed to prime stretch benchmark engine: {error}"));
}

fn bench_gapless_trim(c: &mut Criterion) {
    let rt = make_runtime();
    let temp_dir = TempDir::new().unwrap_or_else(|e| panic!("tempdir failed: {e}"));
    let file_path = temp_dir.path().join("audit-gapless.mp3");
    fs::write(&file_path, test_mp3_bytes())
        .unwrap_or_else(|e| panic!("failed to write bench mp3: {e}"));

    let mut group = c.benchmark_group("audit_gapless_trim");
    group.sampling_mode(SamplingMode::Flat);
    group.sample_size(20);
    group.warm_up_time(Duration::from_secs(1));
    group.measurement_time(Duration::from_secs(4));

    group.bench_function("decode_mp3_to_eof", |b| {
        b.iter(|| {
            rt.block_on(async {
                let pools = pools();
                let file_config = FileConfig::for_src(file_path.clone().into())
                    .store(
                        AssetStore::builder(pools.clone())
                            .backend(StorageBackend::Memory)
                            .build(),
                    )
                    .build();
                let config = AudioConfig::<File<TestPools>>::for_stream(file_config)
                    .hint("mp3".to_string())
                    .build();
                let worker = PlayWorker::new(PlayWorkerConfig::builder(pools).build());
                let mut audio = kithara_integration_tests::mock::load_audio(&worker, config)
                    .await
                    .unwrap_or_else(|e| panic!("audio init failed: {e}"));
                let mut buf = [0.0_f32; 8_192];
                let mut total = 0_usize;
                let mut reached_eof = false;
                for _ in 0..consts::DECODE_READ_LIMIT {
                    match audio.read(&mut buf) {
                        Ok(ReadOutcome::Frames { count, .. }) => total += count.get(),
                        Ok(ReadOutcome::Pending { .. }) => continue,
                        Ok(ReadOutcome::Eof { .. }) => {
                            reached_eof = true;
                            break;
                        }
                        Err(e) => panic!("audio read failed: {e}"),
                    }
                }
                assert!(reached_eof, "decode exceeded its bounded read budget");
                black_box(total);
            });
        });
    });

    group.finish();
}

async fn analyze_track(
    analysis_worker: &AnalysisWorker,
    play_worker: &PlayWorker<TestPools>,
    file_path: &Path,
    store: &AssetStore<TestPools>,
    token: &AnalysisToken,
) -> AnalysisProgress {
    let file_config = FileConfig::for_src(file_path.to_path_buf().into())
        .store(store.clone())
        .build();
    let config = AudioConfig::<File<TestPools>>::for_stream(file_config)
        .hint("mp3".to_owned())
        .build();
    let reader = kithara_integration_tests::mock::load_audio(play_worker, config)
        .await
        .unwrap_or_else(|error| panic!("analysis benchmark reader failed to open: {error}"));
    let rate = reader.spec().sample_rate;
    let (mut results, _producer) = analysis_worker
        .analyze(
            Box::new(reader),
            token.clone(),
            rate,
            0,
            AnalysisDemand::ALL,
        )
        .expect("the analysis pass opens");
    while results.changed().await.is_ok() {}

    let progress = results.borrow().clone();
    progress.unwrap_or_else(|| panic!("analysis benchmark produced no result"))
}

fn assert_complete_analysis(worker: &AnalysisWorker, progress: &AnalysisProgress) -> u64 {
    let analysis = progress.analysis();
    assert!(
        worker.fingerprint().beat().is_some(),
        "benchmark requires an effective beat detector"
    );
    assert!(analysis.is_settled(), "analysis did not reach final state");
    assert!(analysis.waveform().is_some(), "waveform result is missing");
    let beat = analysis
        .beat()
        .unwrap_or_else(|| panic!("beat result is missing"));
    assert!(
        beat.artifact().bpm().is_finite() && beat.artifact().bpm() > 0.0,
        "beat detector produced no effective BPM"
    );
    assert!(
        !beat.artifact().beats().is_empty(),
        "beat detector produced no beat markers"
    );
    assert!(
        !progress.is_resumable(),
        "settled analysis retained resume state"
    );
    analysis
        .extent()
        .filter(|extent| *extent > 0)
        .unwrap_or_else(|| panic!("analysis benchmark result has no source extent"))
}

fn bench_analysis_worker(c: &mut Criterion) {
    let rt = make_runtime();
    let temp_dir = TempDir::new().unwrap_or_else(|e| panic!("tempdir failed: {e}"));
    let file_path = temp_dir.path().join("audit-analysis.mp3");
    fs::write(&file_path, test_mp3_bytes())
        .unwrap_or_else(|e| panic!("failed to write bench mp3: {e}"));
    let pools = pools();
    let store = AssetStore::builder(pools.clone())
        .backend(StorageBackend::Memory)
        .build();
    let play_worker = PlayWorker::new(
        PlayWorkerConfig::builder(pools.clone())
            .runtime(Some(rt.handle().clone()))
            .build(),
    );
    let builder = AnalyzerBuilder::<PlaybackResamplerBackend, TestPools>::new(pools.clone())
        .with_beat_config(BeatAnalysisConfig::default())
        .with_beat()
        .with_waveform(consts::ANALYSIS_BUCKETS);
    let analysis_worker = AnalysisWorker::new(AnalysisWorkerConfig::for_builder(builder).build());
    let token = AnalysisToken::from("perf-audit-track");

    let warm = rt.block_on(analyze_track(
        &analysis_worker,
        &play_worker,
        &file_path,
        &store,
        &token,
    ));
    let source_frames = assert_complete_analysis(&analysis_worker, &warm);

    {
        let mut group = c.benchmark_group("audit_analysis_worker");
        group.sampling_mode(SamplingMode::Flat);
        group.sample_size(20);
        group.warm_up_time(Duration::from_secs(1));
        group.measurement_time(Duration::from_secs(4));
        group.throughput(Throughput::Elements(source_frames));

        group.bench_function("decode_waveform_and_beat_to_settled", |b| {
            b.iter_with_large_drop(|| {
                let progress = rt.block_on(analyze_track(
                    &analysis_worker,
                    &play_worker,
                    black_box(&file_path),
                    &store,
                    &token,
                ));
                assert_complete_analysis(&analysis_worker, &progress);
                black_box(progress)
            });
        });

        group.finish();
    }

    let chunk_frames = NonZeroU64::new(
        u64::from(warm.analysis().source_sample_rate().get()) * consts::ANALYSIS_CHUNK_SECONDS,
    )
    .unwrap_or_else(|| panic!("analysis checkpoint chunk size is zero"));
    let file_spec = AnalysisFileSpec::for_analysis(warm.analysis(), chunk_frames)
        .unwrap_or_else(|error| panic!("analysis checkpoint spec failed: {error}"));
    let mut checkpoint = c.benchmark_group("audit_analysis_checkpoint");
    checkpoint.sampling_mode(SamplingMode::Flat);
    checkpoint.sample_size(50);
    checkpoint.warm_up_time(Duration::from_secs(1));
    checkpoint.measurement_time(Duration::from_secs(4));
    checkpoint.throughput(Throughput::Elements(source_frames));
    checkpoint.bench_function("serialize_full_update", |b| {
        b.iter_with_large_drop(|| {
            black_box(
                AnalysisFile::create(black_box(&file_spec), black_box(&warm))
                    .unwrap_or_else(|error| panic!("analysis checkpoint encoding failed: {error}")),
            )
        });
    });
    checkpoint.finish();
}

fn bench_stretch_prepare(c: &mut Criterion) {
    let pools = pools();
    let mut group = c.benchmark_group("audit_stretch_prepare");
    group.sampling_mode(SamplingMode::Flat);
    group.sample_size(20);
    group.warm_up_time(Duration::from_secs(1));
    group.measurement_time(Duration::from_secs(4));

    for &backend in StretchKind::all() {
        group.bench_with_input(
            BenchmarkId::new("prepare", backend),
            &backend,
            |b, &backend| {
                b.iter_batched(
                    || stretch_config(backend, &pools),
                    |config| {
                        build_engine(config).unwrap_or_else(|error| {
                            panic!("failed to prepare {backend} benchmark engine: {error}")
                        })
                    },
                    BatchSize::PerIteration,
                );
            },
        );
    }

    group.finish();
}

fn bench_stretch_backends(c: &mut Criterion) {
    let pools = pools();
    let steady_request = ElasticRequest::new(consts::STRETCH_FRAMES, consts::STRETCH_FRAMES)
        .unwrap_or_else(|error| panic!("invalid steady stretch request: {error}"));
    let control_requests = [
        ElasticRequest::new(consts::CONTROL_OUTPUT_FRAMES, consts::CONTROL_OUTPUT_FRAMES)
            .unwrap_or_else(|error| panic!("invalid unity control request: {error}")),
        ElasticRequest::new(consts::CONTROL_SOURCE_FRAMES, consts::CONTROL_OUTPUT_FRAMES)
            .unwrap_or_else(|error| panic!("invalid changed control request: {error}")),
    ];
    let steady_source = make_pcm(steady_request.source_frames());
    let control_source = make_pcm(consts::CONTROL_SOURCE_FRAMES);

    let mut group = c.benchmark_group("audit_stretch");
    group.sampling_mode(SamplingMode::Flat);
    group.sample_size(20);
    group.warm_up_time(Duration::from_secs(1));
    group.measurement_time(Duration::from_secs(4));

    for &backend in StretchKind::all() {
        let mut prime_target = stretch_engine(backend, &pools);
        let mut prime_buffers = prime_fixture(prime_target.as_ref());
        group.throughput(frame_throughput(prime_buffers.request.output_frames()));
        group.bench_function(BenchmarkId::new("prime", backend), |b| {
            b.iter(|| {
                prime_target
                    .prime(
                        prime_buffers.request,
                        black_box(&prime_buffers.source_history),
                        black_box(&prime_buffers.source_lookahead),
                        black_box(&prime_buffers.source),
                        black_box(&mut prime_buffers.discarded_output),
                    )
                    .unwrap_or_else(|error| {
                        panic!("failed to prime {backend} benchmark engine: {error}")
                    });
            });
        });
        drop(prime_target);

        let mut steady_target = stretch_engine(backend, &pools);
        let mut steady_prime = prime_fixture(steady_target.as_ref());
        prime_engine(steady_target.as_mut(), &mut steady_prime);
        let mut steady_output = vec![0.0; steady_request.output_frames() * consts::CHANNELS];
        group.throughput(frame_throughput(steady_request.output_frames()));
        group.bench_function(BenchmarkId::new("steady_process", backend), |b| {
            b.iter(|| {
                steady_target
                    .process(
                        steady_request,
                        black_box(&steady_source),
                        black_box(&mut steady_output),
                    )
                    .unwrap_or_else(|error| {
                        panic!("failed to process {backend} steady benchmark block: {error}")
                    });
            });
        });
        drop(steady_target);

        let mut control_target = stretch_engine(backend, &pools);
        let mut control_prime = prime_fixture(control_target.as_ref());
        prime_engine(control_target.as_mut(), &mut control_prime);
        let mut control_output = vec![0.0; consts::CONTROL_OUTPUT_FRAMES * consts::CHANNELS];
        let mut control_index = 0_usize;
        let pitch_scales = [1.0, 1.122_462_048_309_373];
        group.throughput(frame_throughput(consts::CONTROL_OUTPUT_FRAMES));
        group.bench_function(BenchmarkId::new("live_control_change", backend), |b| {
            b.iter(|| {
                control_index ^= 1;
                let request = control_requests[control_index];
                control_target
                    .set_pitch(black_box(pitch_scales[control_index]))
                    .unwrap_or_else(|error| {
                        panic!("failed to set {backend} benchmark pitch: {error}")
                    });
                control_target
                    .process(
                        request,
                        black_box(&control_source[..request.source_frames() * consts::CHANNELS]),
                        black_box(&mut control_output),
                    )
                    .unwrap_or_else(|error| {
                        panic!("failed to process {backend} live-control block: {error}")
                    });
            });
        });
    }

    group.finish();
}

fn bench_stretch_process(c: &mut Criterion) {
    let pcm = make_pcm(consts::STRETCH_FRAMES);
    let pools = pools();
    let spec = AudioSpec::new(
        u16::try_from(consts::CHANNELS).unwrap_or_else(|_| panic!("bench channels")),
        NonZeroU32::new(consts::SAMPLE_RATE).unwrap_or_else(|| panic!("bench sample rate")),
    );

    let mut group = c.benchmark_group("audit_stretch_process");
    group.sample_size(20);
    group.measurement_time(Duration::from_secs(4));
    group.throughput(frame_throughput(consts::STRETCH_FRAMES));

    for &backend in StretchKind::all() {
        let config = WarpConfig::builder()
            .speed(0.8)
            .backend(backend)
            .keylock(true)
            .build();
        let backend_label = backend.to_string().to_ascii_lowercase();
        group.bench_function(format!("{backend_label}_ratio_1_25"), |b| {
            b.iter_batched(
                || {
                    let warp = Warp::new((), &config);
                    let renderer = warp.renderer(spec, pools.clone());
                    let chunk = make_chunk(&pools, &pcm);
                    (renderer, chunk)
                },
                |(mut renderer, chunk): (WarpRenderer<TestPools>, AudioChunk)| {
                    black_box(renderer.render(chunk))
                },
                BatchSize::SmallInput,
            );
        });
    }

    group.finish();
}

criterion_group!(
    benches,
    bench_gapless_trim,
    bench_analysis_worker,
    bench_stretch_prepare,
    bench_stretch_backends,
    bench_stretch_process,
    limiter::bench
);
criterion_main!(benches);
