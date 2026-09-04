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
        AnalysisFile, AnalysisFileSpec, AnalysisProgress, AnalysisToken, AnalysisWorker,
        AnalysisWorkerConfig, AnalyzerBuilder, BeatAnalysisConfig,
    },
    assets::{AssetStore, StorageBackend},
    audio::{AudioConfig, AudioRead, ReadOutcome},
    file::{File, FileConfig},
    platform::{
        sync::Arc,
        time::Duration,
        tokio::runtime::{Builder, Runtime},
    },
    play::{PlayWorker, PlayWorkerConfig, PlaybackResamplerBackend},
    signal::{AudioChunk, AudioChunkInfo, AudioSpec},
    stretch::{ElasticConfig, ElasticEngine, ElasticRequest, StretchKind, build_engine},
    warp::{StretchControls, Warp, WarpConfig, WarpRenderer},
};
use kithara_integration_tests::bufpool_ext::{Pools, TestPools, pools};
use kithara_test_fixtures::assets::signal_mp3_track_sine440_187s;
use num_traits::ToPrimitive;
use tempfile::TempDir;

struct Consts;

impl Consts {
    const CHANNELS: usize = 2;
    const SAMPLE_RATE: u32 = 44_100;
    const ANALYSIS_BUCKETS: usize = 2_000;
    const ANALYSIS_CHUNK_SECONDS: u64 = 16;
    const DECODE_READ_LIMIT: usize = 16_384;
    const STRETCH_FRAMES: usize = 8_192;
    const CONTROL_OUTPUT_FRAMES: usize = 4_096;
    const CONTROL_SOURCE_FRAMES: usize = 4_800;
}

struct PrimeFixture {
    discarded_output: Vec<f32>,
    request: ElasticRequest,
    source: Vec<f32>,
    source_history: Vec<f32>,
    source_lookahead: Vec<f32>,
}

/// The generated full-length MPEG clip the decode benchmarks read.
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
    let mut pcm = Vec::with_capacity(frames * Consts::CHANNELS);
    let sample_rate = Consts::SAMPLE_RATE
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
        u16::try_from(Consts::CHANNELS).unwrap_or_else(|_| panic!("bench channels")),
        NonZeroU32::new(Consts::SAMPLE_RATE).unwrap_or_else(|| panic!("bench sample rate")),
    );
    let mut samples = pools
        .get_with_len::<f32>(pcm.len())
        .unwrap_or_else(|error| panic!("bench PCM allocation failed: {error}"));
    samples.clone_from_slice(pcm);
    AudioChunk::new(
        AudioChunkInfo {
            spec,
            frames: u32::try_from(pcm.len() / Consts::CHANNELS)
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
        .sample_rate(Consts::SAMPLE_RATE)
        .channels(Consts::CHANNELS)
        .max_source_frames(Consts::STRETCH_FRAMES)
        .max_output_frames(Consts::STRETCH_FRAMES)
        .build()
        .unwrap_or_else(|error| panic!("invalid stretch benchmark config: {error}"))
}

fn stretch_engine(backend: StretchKind, pools: &Pools) -> Box<dyn ElasticEngine> {
    build_engine(stretch_config(backend, pools))
        .unwrap_or_else(|error| panic!("failed to prepare {backend} benchmark engine: {error}"))
}

fn prime_fixture(engine: &dyn ElasticEngine) -> PrimeFixture {
    let latency = engine.capabilities().latency();
    let request = ElasticRequest::new(latency.output_frames(), latency.output_frames())
        .unwrap_or_else(|error| panic!("invalid {latency:?} benchmark prime request: {error}"));
    PrimeFixture {
        discarded_output: vec![0.0; request.output_frames() * Consts::CHANNELS],
        request,
        source: make_pcm(request.source_frames()),
        source_history: make_pcm(latency.source_frames()),
        source_lookahead: make_pcm(latency.source_frames()),
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
                    .pools(pools.clone())
                    .build();
                let config = AudioConfig::<File<TestPools>>::for_stream(file_config)
                    .hint("mp3".to_string())
                    .build();
                let worker = PlayWorker::new(PlayWorkerConfig::builder(pools).build());
                let mut audio = worker
                    .open(config)
                    .await
                    .unwrap_or_else(|e| panic!("audio init failed: {e}"));
                let mut buf = [0.0_f32; 8_192];
                let mut total = 0_usize;
                let mut reached_eof = false;
                for _ in 0..Consts::DECODE_READ_LIMIT {
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
    pools: &Pools,
    token: &AnalysisToken,
) -> AnalysisProgress {
    let file_config = FileConfig::for_src(file_path.to_path_buf().into())
        .store(store.clone())
        .pools(pools.clone())
        .build();
    let config = AudioConfig::<File<TestPools>>::for_stream(file_config)
        .hint("mp3".to_owned())
        .build();
    let reader = play_worker
        .open(config)
        .await
        .unwrap_or_else(|error| panic!("analysis benchmark reader failed to open: {error}"));
    let rate = reader.spec().sample_rate;
    let (mut results, _producer) =
        analysis_worker.analyze(Box::new(reader), token.clone(), rate, 0);
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
    let file_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../assets/test.mp3");
    let pools = pools();
    let store = AssetStore::builder(pools.clone())
        .backend(StorageBackend::Memory)
        .build();
    let play_worker = PlayWorker::new(PlayWorkerConfig::builder(pools.clone()).build());
    let builder = AnalyzerBuilder::<PlaybackResamplerBackend, TestPools>::new(pools.clone())
        .with_beat_config(BeatAnalysisConfig::default())
        .with_beat()
        .with_waveform(Consts::ANALYSIS_BUCKETS);
    let analysis_worker = AnalysisWorker::new(AnalysisWorkerConfig::for_builder(builder).build());
    let token = AnalysisToken::from("perf-audit-track");

    let warm = rt.block_on(analyze_track(
        &analysis_worker,
        &play_worker,
        &file_path,
        &store,
        &pools,
        &token,
    ));
    let source_frames = assert_complete_analysis(&analysis_worker, &warm);

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
                &pools,
                &token,
            ));
            assert_complete_analysis(&analysis_worker, &progress);
            black_box(progress)
        });
    });

    group.finish();

    let chunk_frames = NonZeroU64::new(
        u64::from(warm.analysis().source_sample_rate().get()) * Consts::ANALYSIS_CHUNK_SECONDS,
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
    let steady_request = ElasticRequest::new(Consts::STRETCH_FRAMES, Consts::STRETCH_FRAMES)
        .unwrap_or_else(|error| panic!("invalid steady stretch request: {error}"));
    let control_requests = [
        ElasticRequest::new(Consts::CONTROL_OUTPUT_FRAMES, Consts::CONTROL_OUTPUT_FRAMES)
            .unwrap_or_else(|error| panic!("invalid unity control request: {error}")),
        ElasticRequest::new(Consts::CONTROL_SOURCE_FRAMES, Consts::CONTROL_OUTPUT_FRAMES)
            .unwrap_or_else(|error| panic!("invalid changed control request: {error}")),
    ];
    let steady_source = make_pcm(steady_request.source_frames());
    let control_source = make_pcm(Consts::CONTROL_SOURCE_FRAMES);

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
        let mut steady_output = vec![0.0; steady_request.output_frames() * Consts::CHANNELS];
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
        let mut control_output = vec![0.0; Consts::CONTROL_OUTPUT_FRAMES * Consts::CHANNELS];
        let mut control_index = 0_usize;
        let pitch_scales = [1.0, 1.122_462_048_309_373];
        group.throughput(frame_throughput(Consts::CONTROL_OUTPUT_FRAMES));
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
                        black_box(&control_source[..request.source_frames() * Consts::CHANNELS]),
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
    let pcm = make_pcm(Consts::STRETCH_FRAMES);
    let pools = pools();
    let spec = AudioSpec::new(
        u16::try_from(Consts::CHANNELS).unwrap_or_else(|_| panic!("bench channels")),
        NonZeroU32::new(Consts::SAMPLE_RATE).unwrap_or_else(|| panic!("bench sample rate")),
    );

    let mut group = c.benchmark_group("audit_stretch_process");
    group.sample_size(20);
    group.measurement_time(Duration::from_secs(4));
    group.throughput(frame_throughput(Consts::STRETCH_FRAMES));

    for &backend in StretchKind::all() {
        let controls = StretchControls::new(0.8);
        controls.set_backend(backend);
        controls.set_keylock(true);
        let backend_label = backend.to_string().to_ascii_lowercase();
        group.bench_function(format!("{backend_label}_ratio_1_25"), |b| {
            b.iter_batched(
                || {
                    let config = WarpConfig::builder().stretch(Arc::clone(&controls)).build();
                    let warp = Warp::new((), &config);
                    let renderer = warp.renderer(spec, pools.clone());
                    let chunk = make_chunk(&pools, &pcm);
                    (renderer, chunk)
                },
                |(mut renderer, chunk): (WarpRenderer<TestPools>, AudioChunk)| {
                    black_box(renderer.render(chunk));
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
    bench_stretch_process
);
criterion_main!(benches);
