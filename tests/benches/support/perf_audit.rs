struct NativeWork<F> {
    action: F,
}

impl<F: FnMut()> NativeWork<F> {
    fn new(action: F) -> Self {
        Self { action }
    }

    fn run(&mut self) {
        (self.action)();
    }
}

fn backend(index: usize) -> StretchKind {
    StretchKind::all()[index]
}

#[metabench::benchmark(AUDIT_STRETCH_PREPARE, "audit_stretch_prepare", "prepare", gungraun_setup = prepare_engine)]
#[bench::signalsmith(0_usize)]
#[bench::bungee(1_usize)]
fn native_stretch_prepare<F: FnMut()>(mut work: NativeWork<F>) -> NativeWork<F> {
    work.run();
    work
}

fn prepare_engine(index: usize) -> NativeWork<impl FnMut()> {
    let pools = pools();
    NativeWork::new(move || {
        build_engine(stretch_config(backend(index), &pools))
            .unwrap_or_else(|error| panic!("failed to prepare benchmark engine: {error}"));
    })
}

fn prepare_prime(index: usize) -> NativeWork<impl FnMut()> {
    let pools = pools();
    let mut target = stretch_engine(backend(index), &pools);
    let mut buffers = prime_fixture(target.as_ref());
    NativeWork::new(move || {
        target
            .prime(
                buffers.request,
                std::hint::black_box(&buffers.source_history),
                std::hint::black_box(&buffers.source_lookahead),
                std::hint::black_box(&buffers.source),
                std::hint::black_box(&mut buffers.discarded_output),
            )
            .unwrap_or_else(|error| panic!("failed to prime benchmark engine: {error}"));
    })
}

#[metabench::benchmark(AUDIT_STRETCH_PRIME, "audit_stretch", "prime", gungraun_setup = prepare_prime)]
#[bench::signalsmith(0_usize)]
#[bench::bungee(1_usize)]
fn native_stretch_prime<F: FnMut()>(mut work: NativeWork<F>) -> NativeWork<F> {
    work.run();
    work
}

fn prepare_steady(index: usize) -> NativeWork<impl FnMut()> {
    let pools = pools();
    let mut target = stretch_engine(backend(index), &pools);
    let mut prime = prime_fixture(target.as_ref());
    prime_engine(target.as_mut(), &mut prime);
    let request = ElasticRequest::new(consts::STRETCH_FRAMES, consts::STRETCH_FRAMES)
        .expect("steady request");
    let source = make_pcm(request.source_frames());
    let mut output = vec![0.0; request.output_frames() * consts::CHANNELS];
    NativeWork::new(move || {
        target
            .process(
                request,
                std::hint::black_box(&source),
                std::hint::black_box(&mut output),
            )
            .unwrap_or_else(|error| panic!("failed steady process: {error}"));
    })
}

#[metabench::benchmark(AUDIT_STRETCH_STEADY, "audit_stretch", "steady_process", gungraun_setup = prepare_steady)]
#[bench::signalsmith(0_usize)]
#[bench::bungee(1_usize)]
fn native_stretch_steady<F: FnMut()>(mut work: NativeWork<F>) -> NativeWork<F> {
    work.run();
    work
}

fn prepare_control(index: usize) -> NativeWork<impl FnMut()> {
    let pools = pools();
    let mut target = stretch_engine(backend(index), &pools);
    let mut prime = prime_fixture(target.as_ref());
    prime_engine(target.as_mut(), &mut prime);
    let requests = [
        ElasticRequest::new(consts::CONTROL_OUTPUT_FRAMES, consts::CONTROL_OUTPUT_FRAMES)
            .expect("unity control"),
        ElasticRequest::new(consts::CONTROL_SOURCE_FRAMES, consts::CONTROL_OUTPUT_FRAMES)
            .expect("changed control"),
    ];
    let source = make_pcm(consts::CONTROL_SOURCE_FRAMES);
    let mut output = vec![0.0; consts::CONTROL_OUTPUT_FRAMES * consts::CHANNELS];
    let mut control_index = 0_usize;
    NativeWork::new(move || {
        control_index ^= 1;
        let request = requests[control_index];
        target
            .set_pitch(std::hint::black_box(
                [1.0, 1.122_462_048_309_373][control_index],
            ))
            .expect("pitch control");
        target
            .process(
                request,
                std::hint::black_box(&source[..request.source_frames() * consts::CHANNELS]),
                std::hint::black_box(&mut output),
            )
            .expect("control process");
    })
}

#[metabench::benchmark(AUDIT_STRETCH_CONTROL, "audit_stretch", "live_control_change", gungraun_setup = prepare_control)]
#[bench::signalsmith(0_usize)]
#[bench::bungee(1_usize)]
fn native_stretch_control<F: FnMut()>(mut work: NativeWork<F>) -> NativeWork<F> {
    work.run();
    work
}

fn prepare_stretch_process(index: usize) -> NativeWork<impl FnMut()> {
    let pools = pools();
    let pcm = make_pcm(consts::STRETCH_FRAMES);
    let spec = AudioSpec::new(2, NonZeroU32::new(consts::SAMPLE_RATE).expect("rate"));
    let config = WarpConfig::builder()
        .speed(0.8)
        .backend(backend(index))
        .controls(WarpControls::builder().pitch(Pitch::HELD).build())
        .build();
    let mut renderer = Warp::new((), &config).renderer(spec, pools.clone());
    let chunk = make_chunk(&pools, &pcm);
    NativeWork::new(move || {
        let source = render_windows(&mut renderer, &pools, spec, &chunk.samples);
        let start = usize::try_from(source).expect("source fits");
        let held = chunk.frames() - start;
        if held > 0 {
            let frames = renderer
                .prepare_terminal_quantum(held)
                .expect("terminal quantum")
                .get();
            let mut input = make_chunk(&pools, &chunk.samples[start * consts::CHANNELS..]);
            input.meta.frame_offset = source;
            input.meta.frames = u32::try_from(frames).expect("window size");
            std::hint::black_box(
                renderer
                    .render_quantum(input)
                    .continue_value()
                    .expect("terminal input"),
            );
        }
    })
}

#[metabench::benchmark(AUDIT_STRETCH_PROCESS, "audit_stretch_process", "process", gungraun_setup = prepare_stretch_process)]
#[bench::signalsmith_ratio_1_25(0_usize)]
#[bench::bungee_ratio_1_25(1_usize)]
fn native_stretch_process<F: FnMut()>(mut work: NativeWork<F>) -> NativeWork<F> {
    work.run();
    work
}

fn prepare_limiter(index: usize) -> NativeWork<impl FnMut()> {
    let shapes = [
        (1, 0),
        (7, 0),
        (128, 0),
        (255, 0),
        (256, 0),
        (257, 0),
        (257, 7),
        (600, 0),
    ];
    let (frames, tail) = shapes[(index / 2) % shapes.len()];
    let amplitude = if index.is_multiple_of(2) { 0.125 } else { 2.0 };
    let input: [Vec<f32>; 2] = std::array::from_fn(|channel| {
        [1.0, -0.5, 0.75, -1.0, 0.25, -0.75, 0.5]
            .into_iter()
            .cycle()
            .skip(channel)
            .take(frames + channel * tail)
            .map(|sample| sample * amplitude)
            .collect()
    });
    let pools = pools();
    let rate = NonZeroU32::new(48_000).expect("rate");
    let mut limiter = PeakLimiter::new(
        rate,
        rate,
        NonZeroUsize::new(2).expect("stereo"),
        LimiterConfig::default(),
        &pools,
    )
    .expect("limiter");
    let [mut left, mut right] = input.clone();
    limiter.process_planar(&mut [&mut left, &mut right]);
    let reduced = [&left, &right]
        .into_iter()
        .zip(&input)
        .any(|(output, input)| {
            output
                .iter()
                .zip(input)
                .take(frames)
                .any(|(output, input)| output.abs() < input.abs())
        });
    assert_eq!(reduced, amplitude > 1.0);
    NativeWork::new(move || {
        let [mut left, mut right] = input.clone();
        limiter.process_planar(&mut [
            std::hint::black_box(left.as_mut_slice()),
            std::hint::black_box(right.as_mut_slice()),
        ]);
        std::hint::black_box((left.as_slice(), right.as_slice()));
    })
}

#[metabench::benchmark(AUDIT_LIMITER, "limiter_detector", "process", gungraun_setup = prepare_limiter)]
#[bench::quiet_1(0_usize)]
#[bench::overloaded_1(1_usize)]
#[bench::quiet_7(2_usize)]
#[bench::overloaded_7(3_usize)]
#[bench::quiet_128(4_usize)]
#[bench::overloaded_128(5_usize)]
#[bench::quiet_255(6_usize)]
#[bench::overloaded_255(7_usize)]
#[bench::quiet_256(8_usize)]
#[bench::overloaded_256(9_usize)]
#[bench::quiet_257(10_usize)]
#[bench::overloaded_257(11_usize)]
#[bench::quiet_257_tail_7(12_usize)]
#[bench::overloaded_257_tail_7(13_usize)]
#[bench::quiet_600(14_usize)]
#[bench::overloaded_600(15_usize)]
fn native_limiter<F: FnMut()>(mut work: NativeWork<F>) -> NativeWork<F> {
    work.run();
    work
}

fn prepare_source_window(index: usize) -> NativeWork<impl FnMut()> {
    let (speed, frames, terminal) = [
        (0.75, 7, false),
        (0.75, 127, false),
        (0.75, 127, true),
        (1.5, 7, false),
        (1.5, 127, false),
        (1.5, 127, true),
    ][index];
    let pcm = make_pcm(consts::STRETCH_FRAMES);
    let (mut renderer, mut chunk) = sampler_fixture(speed, &pcm, frames, terminal);
    let output = renderer
        .render_quantum(chunk.take().expect("input"))
        .continue_value()
        .expect("rendered input")
        .expect("sampler output");
    let expected_frames = if terminal {
        (17.0_f32 / speed).ceil().to_usize().expect("tail frames")
    } else {
        frames
    };
    assert_eq!(output.frames(), expected_frames);
    let span = output.meta.source_span.expect("exact sampler mapping");
    let (numerator, denominator) = span.source_ratio_at(1).expect("fractional frame");
    assert_ne!(numerator % denominator.get(), 0);
    assert!(output.samples.iter().any(|value| *value != 0.0));
    let (mut renderer, mut chunk) = sampler_fixture(speed, &pcm, frames, terminal);
    NativeWork::new(move || {
        let _ = std::hint::black_box(
            renderer.render_quantum(std::hint::black_box(chunk.take().expect("prepared input"))),
        );
    })
}

#[metabench::benchmark(AUDIT_SOURCE_WINDOW, "audit_source_window", "render", gungraun_setup = prepare_source_window)]
#[bench::fractional_short(0_usize)]
#[bench::fractional_chunk(1_usize)]
#[bench::fractional_tail(2_usize)]
#[bench::downsampled_short(3_usize)]
#[bench::downsampled_chunk(4_usize)]
#[bench::downsampled_tail(5_usize)]
fn native_source_window<F: FnMut()>(mut work: NativeWork<F>) -> NativeWork<F> {
    work.run();
    work
}

struct GaplessWork {
    runtime: Runtime,
    _temp_dir: TempDir,
    file_path: std::path::PathBuf,
}

impl GaplessWork {
    fn run(self) -> Self {
        self.runtime.block_on(async {
            let pools = pools();
            let file_config = FileConfig::for_src(self.file_path.clone().into())
                .store(
                    AssetStore::builder(pools.clone())
                        .backend(StorageBackend::Memory)
                        .build(),
                )
                .build();
            let config = AudioConfig::<File<TestPools>>::for_stream(file_config)
                .hint("mp3".to_owned())
                .build();
            let worker = PlayWorker::new(PlayWorkerConfig::builder(pools).build());
            let mut audio = kithara_integration_tests::mock::load_audio(
                &worker,
                (config, TrackConfig::default()),
            )
            .await
            .expect("audio init");
            let mut buf = [0.0_f32; 8_192];
            let mut total = 0_usize;
            let mut reached_eof = false;
            for _ in 0..consts::DECODE_READ_LIMIT {
                match audio.read(&mut buf) {
                    Ok(ReadOutcome::Frames { count, .. }) => total += count.get(),
                    Ok(ReadOutcome::Pending { .. }) => {}
                    Ok(ReadOutcome::Eof { .. }) => {
                        reached_eof = true;
                        break;
                    }
                    Err(error) => panic!("audio read failed: {error}"),
                }
            }
            assert!(reached_eof, "decode exceeded its bounded read budget");
            std::hint::black_box(total);
        });
        self
    }
}

fn prepare_gapless() -> GaplessWork {
    let runtime = make_runtime();
    let temp_dir = TempDir::new().expect("gapless tempdir");
    let file_path = temp_dir.path().join("audit-gapless.mp3");
    fs::write(&file_path, test_mp3_bytes()).expect("gapless fixture");
    GaplessWork {
        runtime,
        _temp_dir: temp_dir,
        file_path,
    }
}

#[metabench::benchmark(AUDIT_GAPLESS, "audit_gapless_trim", "decode_mp3_to_eof", gungraun_setup = prepare_gapless)]
fn native_gapless(work: GaplessWork) -> GaplessWork {
    work.run()
}

struct AnalysisFixture {
    runtime: Runtime,
    _temp_dir: TempDir,
    file_path: std::path::PathBuf,
    store: AssetStore<TestPools>,
    play_worker: PlayWorker<TestPools>,
    analysis_worker: AnalysisWorker,
    token: AnalysisToken,
    warm: AnalysisProgress,
}

fn analysis_fixture() -> AnalysisFixture {
    let runtime = make_runtime();
    let temp_dir = TempDir::new().expect("analysis tempdir");
    let file_path = temp_dir.path().join("audit-analysis.mp3");
    fs::write(&file_path, test_mp3_bytes()).expect("analysis fixture");
    let pools = pools();
    let store = AssetStore::builder(pools.clone())
        .backend(StorageBackend::Memory)
        .build();
    let play_worker = PlayWorker::new(
        PlayWorkerConfig::builder(pools.clone())
            .runtime(Some(runtime.handle().clone()))
            .build(),
    );
    let builder = AnalyzerBuilder::<PlaybackResamplerBackend, TestPools>::new(pools.clone())
        .with_beat_config(BeatAnalysisConfig::default())
        .with_beat()
        .with_waveform(NonZeroU32::MIN);
    let analysis_worker = AnalysisWorker::new(AnalysisWorkerConfig::for_builder(builder).build());
    let token = AnalysisToken::from("perf-audit-track");
    let warm = runtime.block_on(analyze_track(
        &analysis_worker,
        &play_worker,
        &file_path,
        &store,
        &token,
    ));
    assert_complete_analysis(&analysis_worker, &warm);
    AnalysisFixture {
        runtime,
        _temp_dir: temp_dir,
        file_path,
        store,
        play_worker,
        analysis_worker,
        token,
        warm,
    }
}

impl AnalysisFixture {
    fn run(self) -> Self {
        let progress = self.runtime.block_on(analyze_track(
            &self.analysis_worker,
            &self.play_worker,
            std::hint::black_box(&self.file_path),
            &self.store,
            &self.token,
        ));
        assert_complete_analysis(&self.analysis_worker, &progress);
        std::hint::black_box(progress);
        self
    }
}

fn prepare_analysis() -> AnalysisFixture {
    analysis_fixture()
}

#[metabench::benchmark(AUDIT_ANALYSIS, "audit_analysis_worker", "decode_waveform_and_beat_to_settled", gungraun_setup = prepare_analysis)]
fn native_analysis(work: AnalysisFixture) -> AnalysisFixture {
    work.run()
}

struct AnalysisCheckpointWork {
    fixture: AnalysisFixture,
    source_frames: u64,
    file_spec: AnalysisFileSpec,
}

impl AnalysisCheckpointWork {
    fn run(self) -> Self {
        std::hint::black_box(self.source_frames);
        std::hint::black_box(
            AnalysisFile::create(
                std::hint::black_box(&self.file_spec),
                std::hint::black_box(&self.fixture.warm),
            )
            .expect("checkpoint encoding"),
        );
        self
    }
}

fn prepare_analysis_checkpoint() -> AnalysisCheckpointWork {
    let fixture = analysis_fixture();
    let source_frames = assert_complete_analysis(&fixture.analysis_worker, &fixture.warm);
    let chunk_frames = NonZeroU64::new(
        u64::from(fixture.warm.analysis().source_sample_rate().get())
            * consts::ANALYSIS_CHUNK_SECONDS,
    )
    .expect("checkpoint chunk");
    let file_spec = AnalysisFileSpec::for_analysis(fixture.warm.analysis(), chunk_frames)
        .expect("checkpoint spec");
    AnalysisCheckpointWork {
        fixture,
        source_frames,
        file_spec,
    }
}

#[metabench::benchmark(AUDIT_CHECKPOINT, "audit_analysis_checkpoint", "serialize_full_update", gungraun_setup = prepare_analysis_checkpoint)]
fn native_checkpoint(work: AnalysisCheckpointWork) -> AnalysisCheckpointWork {
    work.run()
}

struct QualitySwitchWork {
    runtime: Runtime,
    helper: TestServerHelper,
    server: CreatedHls,
}

impl QualitySwitchWork {
    fn run(self) -> Self {
        std::hint::black_box(self.runtime.block_on(quality_switch(&self.helper, &self.server)));
        self
    }
}

fn prepare_quality_switch() -> QualitySwitchWork {
    setup_tracing();
    let runtime = make_runtime();
    let (helper, server) = runtime.block_on(async {
        let helper = TestServerHelper::new().await;
        let (init, pcm) = hls_saw_6();
        let server = helper
            .create_hls(
                HlsFixtureBuilder::new()
                    .variant_count(2)
                    .segments_per_variant(6)
                    .segment_size(200_000)
                    .segment_duration_secs(50_000.0 / 44_100.0)
                    .custom_data_per_variant(vec![pcm.into(); 2])
                    .init_data_per_variant(vec![init.into(); 2])
                    .codecs("wav".to_owned())
                    .variant_bandwidths(vec![5_000_000, 1_000_000]),
            )
            .await
            .expect("quality fixture");
        (helper, server)
    });
    QualitySwitchWork {
        runtime,
        helper,
        server,
    }
}

#[metabench::benchmark(AUDIT_QUALITY, "audit_quality_switch", "manual_hls_join", gungraun_setup = prepare_quality_switch)]
fn native_quality(work: QualitySwitchWork) -> QualitySwitchWork {
    work.run()
}

fn warp_transition_action(
    mut renderer: WarpRenderer<TestPools>,
    pools: &Pools,
    spec: AudioSpec,
    pcm: &[f32],
) {
    let mut source = 0;
    let mut produced = 0;
    for block in 0..12 {
        if block == 4 {
            renderer.apply(WarpControlsChange::Pitch(Pitch::default()));
        }
        if block == 8 {
            renderer.apply(WarpControlsChange::Pitch(Pitch::HELD));
        }
        renderer.prepare(spec);
        renderer
            .prepare_engine_latency(spec)
            .expect("transition service");
        let meta = AudioChunkInfo {
            spec,
            frame_offset: source,
            ..AudioChunkInfo::default()
        };
        let frames = renderer
            .prepare_source_window(meta, 65_536, 256)
            .expect("mapped window")
            .get();
        let mut input = make_chunk(pools, &pcm[..frames * consts::CHANNELS]);
        input.meta = AudioChunkInfo {
            frames: u32::try_from(frames).expect("window size"),
            ..meta
        };
        let output = renderer
            .render_quantum(input)
            .continue_value()
            .expect("prepared quantum")
            .expect("mapped output");
        produced += output.frames();
        source += u64::try_from(frames).expect("source advance");
        std::hint::black_box(output);
    }
    drop(renderer);
    let mut replacement = Warp::new(
        (),
        &WarpConfig::builder()
            .speed(0.5)
            .controls(WarpControls::builder().pitch(Pitch::HELD).build())
            .backend(StretchKind::Signalsmith)
            .build(),
    )
    .renderer(spec, pools.clone());
    let resumed = render_windows(&mut replacement, pools, spec, &pcm[..16_384]);
    replacement.prepare(spec);
    replacement.apply(WarpControlsChange::Pitch(Pitch::default()));
    replacement
        .set_speed(SpeedCurve::Constant(1.0), 1)
        .expect("unity replacement rate");
    replacement
        .prepare_engine_latency(spec)
        .expect("replacement service");
    let meta = AudioChunkInfo {
        spec,
        frame_offset: resumed,
        ..AudioChunkInfo::default()
    };
    let frames = replacement
        .prepare_source_window(meta, 128, 128)
        .expect("replacement window")
        .get();
    let start = usize::try_from(resumed).expect("source fits");
    let mut input = make_chunk(
        pools,
        &pcm[start * consts::CHANNELS..(start + frames) * consts::CHANNELS],
    );
    input.meta = AudioChunkInfo {
        frames: u32::try_from(frames).expect("window size"),
        ..meta
    };
    let output = replacement
        .render_quantum(input)
        .continue_value()
        .expect("replacement input")
        .expect("replacement output");
    std::hint::black_box((produced, output));
}

struct WarpTransitionWork {
    renderer: Option<WarpRenderer<TestPools>>,
    pools: Pools,
    spec: AudioSpec,
    pcm: Vec<f32>,
}

impl WarpTransitionWork {
    fn run(mut self) -> Self {
        warp_transition_action(
            self.renderer.take().expect("prepared warp renderer"),
            &self.pools,
            self.spec,
            &self.pcm,
        );
        self
    }
}

fn prepare_warp_transition() -> WarpTransitionWork {
    let pools = pools();
    let pcm = make_pcm(65_536);
    let spec = make_chunk(&pools, &pcm[..2]).spec();
    let renderer = Some(
        Warp::new(
            (),
            &WarpConfig::builder()
                .speed(0.5)
                .controls(WarpControls::builder().pitch(Pitch::HELD).build())
                .backend(StretchKind::Signalsmith)
                .build(),
        )
        .renderer(spec, pools.clone()),
    );
    WarpTransitionWork {
        renderer,
        pools,
        spec,
        pcm,
    }
}

#[metabench::benchmark(AUDIT_WARP, "audit_warp_transition", "mapped_and_replacement", gungraun_setup = prepare_warp_transition)]
fn native_warp(work: WarpTransitionWork) -> WarpTransitionWork {
    work.run()
}
