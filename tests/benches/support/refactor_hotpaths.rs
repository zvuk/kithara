use kithara::play::{CurveShape, GainDb};
struct NativeWork<F> {
    action: F,
}

impl<F: FnMut()> NativeWork<F> {
    fn new(action: F) -> Self {
        Self { action }
    }

    fn run(mut self) -> Self {
        (self.action)();
        self
    }
}

fn prepare_resampler(index: usize) -> NativeWork<impl FnMut()> {
    let (source_rate, target_rate, frames) = [(44_100, 44_100, 4_096), (48_000, 44_100, 8_192)][index];
    let mut resampler = build_resampler(source_rate, target_rate, frames);
    let input = make_planar(frames);
    let mut output = make_output(&*resampler);
    NativeWork::new(move || {
        let frames = process_stereo(&mut *resampler, &input, &mut output);
        std::hint::black_box(frames);
        resampler.reset();
    })
}

#[metabench::benchmark(REFACTOR_RESAMPLER, "refactor_resampler", "case", gungraun_setup = prepare_resampler)]
#[bench::unity_ratio_process(0_usize)]
#[bench::fixed_ratio_process(1_usize)]
fn native_resampler<F: FnMut()>(work: NativeWork<F>) -> NativeWork<F> {
    work.run()
}

fn prepare_audio_file(_: usize) -> NativeWork<impl FnMut()> {
    let rt = make_runtime();
    let temp_dir = TempDir::new().expect("audio tempdir");
    let file_path = temp_dir.path().join("bench.mp3");
    fs::write(&file_path, test_mp3_bytes()).expect("audio fixture");
    NativeWork::new(move || {
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
                .hint("mp3".to_owned())
                .build();
            let worker = PlayWorker::new(PlayWorkerConfig::builder(pools).build());
            let mut audio = kithara_integration_tests::mock::load_audio(
                &worker,
                (config, TrackConfig::default()),
            )
            .await
            .expect("audio init");
            let mut buf = [0.0_f32; 4_096];
            let mut total = 0usize;
            while total < consts::AUDIO_READ_TARGET_SAMPLES {
                match audio.read(&mut buf) {
                    Ok(kithara::audio::ReadOutcome::Frames { count, .. }) => total += count.get(),
                    Ok(kithara::audio::ReadOutcome::Pending { .. }) => {}
                    Ok(kithara::audio::ReadOutcome::Eof { .. }) => break,
                    Err(error) => panic!("audio read failed: {error}"),
                }
            }
            std::hint::black_box(total);
        });
    })
}

#[metabench::benchmark(REFACTOR_AUDIO, "refactor_audio_pipeline", "case", gungraun_setup = prepare_audio_file)]
#[bench::new_and_read(0_usize)]
fn native_audio_file<F: FnMut()>(work: NativeWork<F>) -> NativeWork<F> {
    work.run()
}

fn prepare_hls(_: usize) -> NativeWork<impl FnMut()> {
    let rt = make_runtime();
    let server = rt.block_on(TestHttpServer::new(app()));
    let master_url: Url = server.url("/master.m3u8");
    NativeWork::new(move || {
        let _server = &server;
        let url = master_url.clone();
        rt.block_on(async move {
            let pools = pools();
            let net = NetOptions::builder().pool_max_idle_per_host(8).build();
            let downloader = Downloader::new(
                DownloaderConfig::for_client(HttpClient::new(
                    net,
                    pools.clone(),
                    CancelToken::never(),
                ))
                .runtime(kithara::platform::tokio::runtime::Handle::current())
                .build(),
            );
            let store = AssetStore::builder(pools.clone())
                .backend(StorageBackend::Memory)
                .max_bytes(200_000)
                .build();
            let config = HlsConfig::for_url(url)
                .store(store)
                .pools(pools)
                .initial_abr_mode(auto(1))
                .downloader(downloader)
                .download_batch_size(3)
                .look_ahead_bytes(96_000)
                .build();
            let mut stream = Stream::<Hls<TestPools>>::new(config)
                .await
                .expect("stream init");
            let mut buf = [0_u8; 8_192];
            let mut total = 0usize;
            while total < consts::HLS_READ_TARGET_BYTES {
                let n = stream.read(&mut buf).expect("stream read");
                if n == 0 {
                    break;
                }
                total += n;
            }
            for seek_pos in consts::HLS_SEEK_POSITIONS {
                if let Some(len) = stream.len()
                    && seek_pos > len
                {
                    continue;
                }
                stream.seek(SeekFrom::Start(seek_pos)).expect("seek");
                let n = stream.read(&mut buf).expect("read after seek");
                std::hint::black_box(n);
            }
            std::hint::black_box(total);
        });
    })
}

#[metabench::benchmark(REFACTOR_HLS, "refactor_hls_downloader", "case", gungraun_setup = prepare_hls)]
#[bench::new_seek_read_cycle(0_usize)]
fn native_hls<F: FnMut()>(work: NativeWork<F>) -> NativeWork<F> {
    work.run()
}

fn prepare_layout_interleave(channels: u16) -> NativeWork<impl FnMut()> {
    let pools = pools();
    let frames = FrameCount::new(LAYOUT_FRAMES);
    let spec = AudioSpec::new(channels, LAYOUT_RATE);
    let width = usize::from(channels);
    let planar = PlanarBuffer::new(&pools, spec, frames).expect("planar");
    let mut interleaved = vec![0.0_f32; LAYOUT_FRAMES * width];
    NativeWork::new(move || {
        planar
            .view()
            .interleave_into(std::hint::black_box(&mut interleaved))
            .expect("interleave");
    })
}

fn prepare_layout_deinterleave(channels: u16) -> NativeWork<impl FnMut()> {
    let frames = FrameCount::new(LAYOUT_FRAMES);
    let spec = AudioSpec::new(channels, LAYOUT_RATE);
    let width = usize::from(channels);
    let source = vec![0.25_f32; LAYOUT_FRAMES * width];
    let mut planes = vec![vec![0.0_f32; LAYOUT_FRAMES]; width];
    NativeWork::new(move || {
        let view = InterleavedView::new(&source, spec, frames).expect("interleaved view");
        let mut destinations: Vec<&mut [f32]> = planes.iter_mut().map(Vec::as_mut_slice).collect();
        view.deinterleave_channels_into_at(std::hint::black_box(&mut destinations), 0)
            .expect("deinterleave");
    })
}

#[metabench::benchmark(REFACTOR_LAYOUT, "refactor_layout", "case", gungraun_setup = prepare_layout_interleave)]
#[bench::interleave_2ch(2_u16)]
#[bench::deinterleave_2ch(args = (2_u16), setup = prepare_layout_deinterleave)]
#[bench::interleave_6ch(6_u16)]
#[bench::deinterleave_6ch(args = (6_u16), setup = prepare_layout_deinterleave)]
fn native_layout<F: FnMut()>(work: NativeWork<F>) -> NativeWork<F> {
    work.run()
}

fn prepare_render_player(index: usize) -> NativeWork<impl FnMut()> {
    let (label, frames) = [("settled", 128), ("moving", 257), ("seek_tail", 1023)][index];
    let runtime = make_runtime();
    let player = runtime.block_on(render_player(SessionDuckingMode::Soft));
    runtime.block_on(render_buffer_cases::admit_player(&player, label, frames));
    let mut high = false;
    NativeWork::new(move || {
        let output = runtime.block_on(async {
            if label == "moving" {
                high = !high;
                player
                    .set_volume(if high { 0.75 } else { 0.5 })
                    .await
                    .expect("gain change");
            } else if label == "seek_tail" {
                player.seek(0.0).await;
            }
            player.render(frames).await
        });
        std::hint::black_box(output);
        assert_eq!(player.metrics().decode_errors(), 0);
        let output = runtime.block_on(player.render(frames));
        assert!(output.iter().any(|sample| sample.abs() > 0.0));
    })
}

#[metabench::benchmark(REFACTOR_RENDER_PLAYER, "render_buffers", "case", gungraun_setup = prepare_render_player)]
#[bench::settled_128(0_usize)]
#[bench::moving_257(1_usize)]
#[bench::seek_tail_1023(2_usize)]
fn native_render_player<F: FnMut()>(work: NativeWork<F>) -> NativeWork<F> {
    work.run()
}

fn prepare_deck_eq(index: usize) -> NativeWork<impl FnMut()> {
    let rt = make_runtime();
    let harness = rt.block_on(deck_eq_curves::player());
    let (shape, frames) = [
        (CurveShape::HOLD, 13),
        (CurveShape::HOLD, 256),
        (CurveShape::LINEAR, 13),
        (CurveShape::LINEAR, 256),
        (CurveShape::cubic(0.0, 1.0).expect("S shape"), 13),
        (CurveShape::cubic(0.0, 1.0).expect("S shape"), 256),
    ][index];
    rt.block_on(async {
        deck_eq_curves::reset(&harness).await;
        deck_eq_curves::submit(&harness, shape, frames).await;
        let prelude = harness.render(deck_eq_curves::PRELUDE).await;
        assert!(prelude.iter().all(|sample| *sample == 0.5));
    });
    let oracle: Vec<_> = (0..frames)
        .flat_map(|frame| deck_eq_curves::expected(frame, frames, shape).1)
        .collect();
    NativeWork::new(move || {
        let last = rt.block_on(harness.render(frames));
        assert_eq!(last, oracle);
        assert_eq!(
            harness.deck_snapshot().eq.gain(0),
            Some(deck_eq_curves::expected(frames - 1, frames, shape).0)
        );
        assert!(harness.deck_snapshot().eq.program(0).is_some());
        assert_eq!(harness.metrics().underruns(), 0);
    })
}

fn prepare_deck_eq_public() -> NativeWork<impl FnMut()> {
    let rt = make_runtime();
    let harness = rt.block_on(deck_eq_curves::player());
    let shape = CurveShape::cubic(0.0, 1.0).expect("S shape");
    rt.block_on(deck_eq_curves::reset(&harness));
    let oracle: Vec<_> = (0..1024_usize)
        .flat_map(|frame| {
            if frame < deck_eq_curves::PRELUDE {
                [0.5; 2]
            } else {
                deck_eq_curves::expected(frame - deck_eq_curves::PRELUDE, 128, shape).1
            }
        })
        .collect();
    NativeWork::new(move || {
        let last = rt.block_on(async {
            deck_eq_curves::submit(&harness, shape, 128).await;
            harness.render(1024).await
        });
        assert_eq!(last, oracle);
        assert_eq!(harness.deck_snapshot().eq.gain(0), GainDb::checked(-12.0));
        assert_eq!(harness.metrics().underruns(), 0);
    })
}

#[metabench::benchmark(REFACTOR_DECK_EQ, "deck_eq_curves", "case", gungraun_setup = prepare_deck_eq)]
#[bench::render_hold_13(0_usize)]
#[bench::render_hold_256(1_usize)]
#[bench::render_linear_13(2_usize)]
#[bench::render_linear_256(3_usize)]
#[bench::render_cubic_13(4_usize)]
#[bench::render_cubic_256(5_usize)]
#[bench::public_host_cubic_1024(setup = prepare_deck_eq_public)]
fn native_deck_eq<F: FnMut()>(work: NativeWork<F>) -> NativeWork<F> {
    work.run()
}

fn prepare_filter_sweep(index: usize) -> NativeWork<impl FnMut()> {
    let rt = make_runtime();
    let harness = rt.block_on(slot_gain::loaded_player());
    let (shape, frames, public) = [
        (CurveShape::HOLD, 13, false),
        (CurveShape::HOLD, 256, false),
        (CurveShape::LINEAR, 13, false),
        (CurveShape::LINEAR, 256, false),
        (CurveShape::cubic(0.0, 1.0).expect("S shape"), 13, false),
        (CurveShape::cubic(0.0, 1.0).expect("S shape"), 256, false),
        (CurveShape::cubic(0.0, 1.0).expect("S shape"), 128, true),
    ][index];
    let (expected, target) = rt.block_on(filter_sweep::oracle(
        &harness,
        shape,
        frames,
        if public { filter_sweep::PRELUDE } else { 0 },
    ));
    rt.block_on(async {
        filter_sweep::prepare(&harness).await;
        if !public {
            filter_sweep::submit(&harness, shape, frames).await;
            let _ = harness.render(filter_sweep::PRELUDE).await;
        }
    });
    NativeWork::new(move || {
        let last = rt.block_on(async {
            if public {
                filter_sweep::submit(&harness, shape, frames).await;
            }
            harness.render(if public { 1024 } else { frames }).await
        });
        assert_eq!(last, expected);
        let snapshot = harness.deck_snapshot().filter;
        assert_eq!(snapshot.knob().to_bits(), target.to_bits());
        assert!(snapshot.program().is_some());
        assert_eq!(harness.metrics().underruns(), 0);
    })
}

#[metabench::benchmark(REFACTOR_FILTER_SWEEP, "filter_sweep", "case", gungraun_setup = prepare_filter_sweep)]
#[bench::render_hold_13(0_usize)]
#[bench::render_hold_256(1_usize)]
#[bench::render_linear_13(2_usize)]
#[bench::render_linear_256(3_usize)]
#[bench::render_cubic_13(4_usize)]
#[bench::render_cubic_256(5_usize)]
#[bench::public_host_cubic_1024(6_usize)]
fn native_filter_sweep<F: FnMut()>(work: NativeWork<F>) -> NativeWork<F> {
    work.run()
}

fn prepare_slot_gain_host(_: usize) -> NativeWork<impl FnMut()> {
    let rt = make_runtime();
    let harness = rt.block_on(slot_gain::loaded_player());
    let item = rt.block_on(harness.with_queue(|queue| queue.current().expect("loaded item").id));
    rt.block_on(async {
        harness.seek(0.0).await;
        let _ = harness.render(slot_gain::WARM_FRAMES).await;
        let warm = slot_gain::gain_window(&harness, item, 1.0).await;
        assert!(warm[warm.len() - 128..].iter().all(|sample| *sample == 0.5));
    });
    let expected: Vec<_> = (0..1024_u16)
        .flat_map(|at| {
            use num_traits::ToPrimitive;
            let gain = match at {
                0..529 => 1.0,
                529..657 => {
                    let time = f64::from(at - 529) / 128.0;
                    1.0 - 0.75 * (3.0 * time * time - 2.0 * time * time * time)
                }
                _ => 0.25,
            };
            [0.5 * gain.to_f32().expect("bounded amplitude"); 2]
        })
        .collect();
    NativeWork::new(move || {
        let last = rt.block_on(slot_gain::gain_window(&harness, item, 0.25));
        assert_eq!(last, expected);
    })
}

#[metabench::benchmark(REFACTOR_SLOT_HOST, "slot_gain_curves", "case", gungraun_setup = prepare_slot_gain_host)]
#[bench::public_host_cubic_1024(0_usize)]
fn native_slot_host<F: FnMut()>(work: NativeWork<F>) -> NativeWork<F> {
    work.run()
}

fn prepare_slot_shape(frames: u16) -> NativeWork<impl FnMut()> {
    let shape = CurveShape::cubic(0.0, 1.0).expect("S shape");
    let positions: Vec<_> = (256 - frames..256)
        .map(|at| f64::from(at) / 256.0)
        .collect();
    let expected: Vec<_> = positions
        .iter()
        .map(|&time| 3.0 * time * time - 2.0 * time * time * time)
        .collect();
    let mut last = vec![0.0; usize::from(frames)];
    NativeWork::new(move || {
        last.copy_from_slice(std::hint::black_box(&positions));
        shape.process(&mut last, [0.0, 1.0]);
        assert_eq!(last, expected);
    })
}

#[metabench::benchmark(REFACTOR_SLOT_SHAPE, "slot_gain_curves", "case", gungraun_setup = prepare_slot_shape)]
#[bench::shape_cubic_13(13_u16)]
#[bench::shape_cubic_256(256_u16)]
fn native_slot_shape<F: FnMut()>(work: NativeWork<F>) -> NativeWork<F> {
    work.run()
}

fn prepare_filter_chain(index: usize) -> NativeWork<impl FnMut()> {
    let pools = pools();
    let rate = NonZeroU32::new(48_000).expect("rate");
    let mut filter = filter_chain::prepared_filter(rate);
    let frames = [0, 7, 128, 257][index];
    let fixture = make_pcm(frames, 2);
    let mut planes = filter_chain::input_planes(&pools, &fixture);
    NativeWork::new(move || {
        filter.process(std::hint::black_box(&mut planes), 0..frames).expect("planes");
    })
}

fn prepare_filter_chain_burst() -> NativeWork<impl FnMut()> {
    let pools = pools();
    let rate = NonZeroU32::new(48_000).expect("rate");
    let mut filter = filter_chain::prepared_filter(rate);
    let mut burst = vec![0.0; 514];
    burst[..64].fill(0.25);
    let silence = vec![0.0; 514];
    let mut inputs = [
        filter_chain::input_planes(&pools, &burst),
        filter_chain::input_planes(&pools, &silence),
        filter_chain::input_planes(&pools, &silence),
    ];
    NativeWork::new(move || {
        for planes in &mut inputs {
            filter.process(std::hint::black_box(planes), 0..257).expect("planes");
        }
    })
}

#[metabench::benchmark(REFACTOR_FILTER_CHAIN, "filter_chain", "case", gungraun_setup = prepare_filter_chain)]
#[bench::lowpass_stereo_empty(0_usize)]
#[bench::lowpass_stereo_7(1_usize)]
#[bench::lowpass_stereo_128(2_usize)]
#[bench::lowpass_stereo_257(3_usize)]
#[bench::lowpass_burst_and_two_quiet_blocks_stereo_257(setup = prepare_filter_chain_burst)]
fn native_filter_chain<F: FnMut()>(work: NativeWork<F>) -> NativeWork<F> {
    work.run()
}

fn prepare_adopt(_: usize) -> NativeWork<impl FnMut()> {
    let mut fixture = render_buffer_cases::adopt_fixture();
    NativeWork::new(move || {
        let output = fixture.render(257);
        render_buffer_cases::check_adopt(&mut fixture, &output);
    })
}

#[metabench::benchmark(REFACTOR_ADOPT, "render_buffers", "case", gungraun_setup = prepare_adopt)]
#[bench::adopt_partial_tail_257(0_usize)]
fn native_adopt<F: FnMut()>(work: NativeWork<F>) -> NativeWork<F> {
    work.run()
}

fn prepare_refill(_: usize) -> NativeWork<impl FnMut()> {
    let (mut fixture, gain) = render_buffer_cases::refill_fixture();
    NativeWork::new(move || {
        let output = fixture.render(16);
        render_buffer_cases::check_refill(&mut fixture, gain, &output);
    })
}

#[metabench::benchmark(REFACTOR_REFILL, "render_buffers", "case", gungraun_setup = prepare_refill)]
#[bench::slot_refill_16(0_usize)]
fn native_refill<F: FnMut()>(work: NativeWork<F>) -> NativeWork<F> {
    work.run()
}

fn prepare_jump(_: usize) -> NativeWork<impl FnMut()> {
    let runtime = make_runtime();
    let mut fixture = runtime.block_on(render_buffer_cases::jump_fixture());
    NativeWork::new(move || {
        let output = fixture.run();
        render_buffer_cases::check_jump(&mut fixture, &output);
    })
}

#[metabench::benchmark(REFACTOR_JUMP, "render_buffers", "case", gungraun_setup = prepare_jump)]
#[bench::jump_240(0_usize)]
fn native_jump<F: FnMut()>(work: NativeWork<F>) -> NativeWork<F> {
    work.run()
}

fn prepare_duck(_: usize) -> NativeWork<impl FnMut()> {
    let runtime = make_runtime();
    let player = runtime.block_on(render_player(SessionDuckingMode::Off));
    runtime.block_on(render_buffer_cases::prepare_duck(&player));
    NativeWork::new(move || {
        let output = runtime.block_on(player.render(513));
        render_buffer_cases::check_duck(&output);
    })
}

#[metabench::benchmark(REFACTOR_DUCK, "render_buffers", "case", gungraun_setup = prepare_duck)]
#[bench::duck_spans_513(0_usize)]
fn native_duck<F: FnMut()>(work: NativeWork<F>) -> NativeWork<F> {
    work.run()
}
