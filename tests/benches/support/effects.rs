use std::{
    hint::black_box,
    num::{NonZeroU32, NonZeroUsize},
};

use criterion::{BatchSize, Criterion};
use kithara::{
    assets::AssetStore,
    audio::mock::TestPcmReader,
    effects::{
        AudioEffect, GainDb,
        eq::{EqConfig, EqEffect, generate_log_spaced_bands},
    },
    play::{ResourceConfig, ResourceSrc},
    queue::TrackSource,
    signal::{AudioChunk, AudioChunkInfo, AudioSpec},
    warp::WarpConfig,
};
use kithara_integration_tests::{
    bufpool_ext::{pools, sample_buffer},
    offline::{OfflinePlayer, OfflinePlayerOptions},
};
use kithara_test_fixtures::integration_fixtures::benchmark_half;

const RATE: u32 = 44_100;
const WARM_FRAMES: usize = 4096;
const TAKE_FRAMES: usize = 640;

fn spec() -> AudioSpec {
    AudioSpec::new(2, NonZeroU32::new(RATE).expect("nonzero fixture rate"))
}

pub(super) fn controls(c: &mut Criterion) {
    let region = pools();
    let mut group = c.benchmark_group("refactor_effect_controls");
    group.sample_size(20);
    for frames in [13, 256] {
        let input = super::make_pcm(frames, 2);
        for (label, step, reset) in [
            ("manual_eq_unchanged", false, false),
            ("manual_eq_step", true, false),
            ("unity_eq_reset", false, true),
        ] {
            let mut eq = EqEffect::new(
                &EqConfig::builder(region.clone()).build(),
                generate_log_spaced_bands(3),
                RATE,
                2,
            )
            .expect("pool fits three EQ bands");
            let mut positive = false;
            let mut chunk = Some(AudioChunk::new(
                AudioChunkInfo {
                    spec: spec(),
                    frames: u32::try_from(frames).expect("fixture frames fit u32"),
                    ..Default::default()
                },
                sample_buffer(&region, &input),
            ));
            group.bench_function(format!("{label}/{frames}"), |b| {
                b.iter(|| {
                    let mut pcm = chunk.take().expect("EQ preserves its input chunk");
                    pcm.samples.copy_from_slice(black_box(&input));
                    if step {
                        positive = !positive;
                        eq.set_gain(1, GainDb::from(if positive { 6.0 } else { -6.0 }));
                    }
                    if reset {
                        eq.reset();
                    }
                    chunk = eq.process(pcm);
                    black_box(
                        chunk
                            .as_ref()
                            .expect("EQ emits every chunk")
                            .samples
                            .as_ref(),
                    );
                });
                let output = &chunk.as_ref().expect("EQ emits every chunk").samples;
                assert_eq!(output.len(), input.len());
                assert!(output.iter().all(|sample| sample.is_finite()));
                if reset || !step {
                    assert_eq!(output.as_ref(), input.as_slice());
                }
            });
        }
    }
    group.finish();
}

async fn player_fixture() -> (OfflinePlayer, String) {
    let harness = OfflinePlayer::with_sample_rate(
        OfflinePlayerOptions::builder()
            .block_on_underrun(true)
            .crossfade_duration(0.0)
            .output_block_frames(NonZeroU32::new(64).expect("nonzero callback size"))
            .warp(
                WarpConfig::builder()
                    .render_quantum_frames(NonZeroUsize::new(64).expect("nonzero quantum"))
                    .build(),
            )
            .build(),
        RATE,
    )
    .await;
    let TrackSource::Uri(uri) = harness.pcm_deck(Box::new(TestPcmReader::with_pcm(
        spec(),
        600.0,
        benchmark_half(),
    ))) else {
        panic!("prepared PCM fixture is a URI")
    };
    (harness, uri)
}

async fn prepare_window(harness: &OfflinePlayer) {
    harness.seek(0.0).await;
    let warm = harness.render(WARM_FRAMES).await;
    assert!(warm[warm.len() - 128..].iter().all(|sample| *sample == 0.5));
}

pub(super) fn empty_route(c: &mut Criterion) {
    let rt = super::make_runtime();
    let harness = rt.block_on(async {
        let (harness, uri) = player_fixture().await;
        let config = ResourceConfig::for_src(ResourceSrc::parse(uri).expect("fixture URI parses"))
            .store(AssetStore::builder(harness.worker().pools().clone()).build())
            .preload_chunks(NonZeroUsize::MIN)
            .audio_buffer_chunks(NonZeroUsize::new(2).expect("nonzero buffer chunks"))
            .build();
        harness.load_config(config).await;
        let _ = harness.render(WARM_FRAMES).await;
        harness
    });
    let mut last = Vec::new();
    {
        let mut group = c.benchmark_group("refactor_effect_controls");
        group.sample_size(20);
        group.bench_function("empty_route/640", |b| {
            b.iter_batched(
                || rt.block_on(prepare_window(&harness)),
                |()| {
                    last = rt.block_on(harness.render(TAKE_FRAMES));
                    black_box(&last);
                },
                BatchSize::PerIteration,
            );
            assert_eq!(last, vec![0.5; TAKE_FRAMES * 2]);
        });
        group.finish();
    }
    let item = rt.block_on(harness.with_queue(|queue| queue.current().expect("loaded item").id));
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
    {
        let mut group = c.benchmark_group("slot_gain_curves");
        group.sample_size(20);
        group.bench_function("public_host_cubic/1024", |b| {
            b.iter_batched(
                || {
                    rt.block_on(async {
                        harness.seek(0.0).await;
                        let _ = harness.render(WARM_FRAMES).await;
                        let warm = gain_window(&harness, item, 1.0).await;
                        assert!(warm[warm.len() - 128..].iter().all(|sample| *sample == 0.5));
                    });
                },
                |()| {
                    last = rt.block_on(gain_window(&harness, item, 0.25));
                    black_box(&last);
                },
                BatchSize::PerIteration,
            );
            assert_eq!(last, expected);
        });
        group.finish();
    }
    rt.block_on(harness.close());
}

async fn gain_window(
    harness: &OfflinePlayer,
    item: kithara_events::TrackId,
    target: f64,
) -> Vec<f32> {
    use kithara::play::{CurveShape, ParameterKnot, SlotGainProgram};
    let start = harness.host().position() + 529;
    let program = SlotGainProgram::new(
        [(start, 1.0), (start + 128, target)]
            .into_iter()
            .map(|(at, value)| {
                ParameterKnot::new(
                    SessionFrame::new(i64::try_from(at).expect("gain frame")),
                    value,
                    CurveShape::cubic(0.0, 1.0).expect("S shape"),
                )
                .expect("gain knot")
            })
            .collect(),
    )
    .expect("prepared gain program");
    harness
        .with_queue(move |queue| queue.gain_program(item, program))
        .await
        .expect("reachable output curve");
    harness.render(1024).await
}

pub(super) fn gain_shape(c: &mut Criterion) {
    use kithara::play::CurveShape;
    let shape = CurveShape::cubic(0.0, 1.0).expect("S shape");
    let mut group = c.benchmark_group("slot_gain_curves");
    group.sample_size(20);
    for frames in [13_u16, 256] {
        let positions: Vec<_> = (256 - frames..256)
            .map(|at| f64::from(at) / 256.0)
            .collect();
        let expected: Vec<_> = positions
            .iter()
            .map(|&time| 3.0 * time * time - 2.0 * time * time * time)
            .collect();
        let mut last = vec![0.0; usize::from(frames)];
        group.bench_function(format!("shape_cubic/{frames}"), |b| {
            b.iter(|| {
                for (value, &position) in last.iter_mut().zip(black_box(&positions)) {
                    *value = shape.progress(position);
                }
                black_box(&last);
            });
            assert_eq!(last, expected);
        });
    }
    group.finish();
}

use kithara::{
    effects::{
        EffectConfig,
        eq::{EqBandConfigChange, EqBandConfigControl},
    },
    signal::SessionFrame,
};
use kithara_command::When;

async fn applied_step<C: EqBandConfigControl<At = When<SessionFrame>> + Clone + Send + 'static>(
    harness: &OfflinePlayer,
    control: &C,
) -> Vec<f32> {
    let at = harness.host().position() + 529;
    let at = SessionFrame::new(i64::try_from(at).expect("fixture position fits i64"));
    harness
        .run(control, move |control| {
            assert!(
                control
                    .configure(EqBandConfigChange::GainDb(GainDb::from(6.0)), When::At(at))
                    .is_ok()
            );
        })
        .await;
    let output = harness.render(TAKE_FRAMES).await;
    assert_eq!(control.gain_db(), GainDb::from(6.0));
    output
}

pub(super) fn configured_route(c: &mut Criterion) {
    let rt = super::make_runtime();
    let harness = rt.block_on(async {
        let (harness, uri) = player_fixture().await;
        let config = ResourceConfig::for_src(ResourceSrc::parse(uri).expect("fixture URI parses"))
            .store(AssetStore::builder(harness.worker().pools().clone()).build())
            .preload_chunks(NonZeroUsize::MIN)
            .audio_buffer_chunks(NonZeroUsize::new(2).expect("nonzero buffer chunks"))
            .effects(vec![EffectConfig::Equalizer(generate_log_spaced_bands(3))])
            .build();
        harness.load_config(config).await;
        let _ = harness.render(WARM_FRAMES).await;
        harness
    });
    let control = rt
        .block_on(harness.with_queue(|queue| queue.eq_band(0, 1)))
        .expect("configured track has an EQ band control");
    let mut last = Vec::new();
    {
        let mut group = c.benchmark_group("refactor_effect_newroute");
        group.sample_size(20);
        group.bench_function("configured_unchanged/640", |b| {
            b.iter_batched(
                || rt.block_on(prepare_window(&harness)),
                |()| {
                    last = rt.block_on(harness.render(TAKE_FRAMES));
                    black_box(&last);
                },
                BatchSize::PerIteration,
            );
            assert_eq!(last, vec![0.5; TAKE_FRAMES * 2]);
            assert_eq!(control.gain_db(), GainDb::DEFAULT);
        });
        group.bench_function("configured_applied_step/640", |b| {
            b.iter_batched(
                || {
                    rt.block_on(async {
                        harness
                            .run(&control, |control| control.set_gain_db(GainDb::DEFAULT))
                            .await
                            .expect("unity target is admitted");
                        let _ = harness.render(TAKE_FRAMES).await;
                        assert_eq!(control.gain_db(), GainDb::DEFAULT);
                        prepare_window(&harness).await;
                    });
                },
                |()| {
                    last = rt.block_on(applied_step(&harness, &control));
                    black_box(&last);
                },
                BatchSize::PerIteration,
            );
            assert_eq!(last.len(), TAKE_FRAMES * 2);
            assert!(last[..529 * 2].iter().all(|sample| *sample == 0.5));
            assert!(last[529 * 2..].iter().all(|sample| sample.is_finite()));
            assert!(last[529 * 2..].iter().any(|sample| *sample != 0.5));
        });
        group.finish();
    }
    rt.block_on(harness.close());

    let region = pools();
    let mut bands = generate_log_spaced_bands(3);
    bands[1].set_gain_db(GainDb::from(6.0));
    let mut eq = EqEffect::new(&EqConfig::builder(region).build(), bands, RATE, 2)
        .expect("pool fits three EQ bands");
    let mut group = c.benchmark_group("refactor_effect_newroute");
    group.sample_size(20);
    group.bench_function("nonunity_target_retaining_reset", |b| {
        b.iter(|| {
            eq.reset();
            black_box(&eq);
        });
        assert_eq!(eq.target_gain(1), Some(GainDb::from(6.0)));
    });
    group.finish();
}

pub(super) fn curves(c: &mut Criterion) {
    use kithara::effects::{
        EffectAutomation, EffectParameter, apply_automated_effects, eq::EqBandConfig,
    };
    use kithara_dsp::curve::CurveShape;
    use num_traits::ToPrimitive;

    let region = pools();
    let mut group = c.benchmark_group("effect_curves");
    group.sample_size(20);
    for frames in [13_u16, 256] {
        for kind in 0..3 {
            let (label, shape) = match kind {
                0 => ("hold", CurveShape::HOLD),
                1 => ("linear", CurveShape::LINEAR),
                _ => ("cubic", CurveShape::cubic(0.0, 1.0).expect("S shape")),
            };
            let eq = EqEffect::new(
                &EqConfig::builder(region.clone()).build(),
                vec![EqBandConfig::default()],
                RATE,
                2,
            )
            .expect("pool fits a one-band EQ");
            let mut effects: Vec<Box<dyn AudioEffect>> = vec![Box::new(eq)];
            let input = vec![0.5; usize::from(frames) * 2];
            let mut chunk = Some(AudioChunk::new(
                AudioChunkInfo {
                    spec: spec(),
                    ..Default::default()
                },
                sample_buffer(&region, &input),
            ));
            let controls = [EffectAutomation::new(
                EffectParameter::EqGain { effect: 0, band: 0 },
                -12.0,
                6.0,
                shape,
                0,
                u64::from(frames),
            )
            .expect("prepared segment")];
            let expected: Vec<_> = (0..frames)
                .flat_map(|at| {
                    let time = f64::from(at) / f64::from(frames);
                    let progress = match kind {
                        0 => 0.0,
                        1 => time,
                        _ => 3.0 * time * time - 2.0 * time * time * time,
                    };
                    let gain_db = (-12.0 + 18.0 * progress).to_f32().expect("EQ range");
                    [0.5 * 10.0_f32.powf(gain_db / 20.0); 2]
                })
                .collect();
            group.bench_function(format!("chain_{label}/{frames}"), |b| {
                b.iter(|| {
                    let mut pcm = chunk.take().expect("frame-preserving chain");
                    pcm.samples.copy_from_slice(black_box(&input));
                    chunk = apply_automated_effects(&mut effects, pcm, black_box(&controls))
                        .expect("admitted parameter block");
                    black_box(&chunk);
                });
                assert_eq!(
                    chunk.as_ref().expect("output chunk").samples.as_ref(),
                    expected
                );
            });
        }
    }
    group.finish();
}

pub(super) fn curve_host(c: &mut Criterion) {
    use kithara::{
        effects::eq::{EqBandConfig, EqBandConfigControl},
        play::{CurveShape, ParameterKnot},
    };
    use num_traits::ToPrimitive;

    let rt = super::make_runtime();
    let harness = rt.block_on(async {
        let (harness, uri) = player_fixture().await;
        let config = ResourceConfig::for_src(ResourceSrc::parse(uri).expect("fixture URI"))
            .store(AssetStore::builder(harness.worker().pools().clone()).build())
            .preload_chunks(NonZeroUsize::MIN)
            .audio_buffer_chunks(NonZeroUsize::new(2).expect("buffer chunks"))
            .effects(vec![EffectConfig::Equalizer(vec![EqBandConfig::default()])])
            .build();
        harness.load_config(config).await;
        let _ = harness.render(WARM_FRAMES).await;
        harness
    });
    let control = rt
        .block_on(harness.with_queue(|queue| queue.eq_band(0, 0)))
        .expect("bound EQ field");
    let mut last = Vec::new();
    {
        let mut group = c.benchmark_group("effect_curves");
        group.sample_size(20);
        group.bench_function("public_host_cubic/1024", |b| {
            b.iter_batched(
                || {
                    rt.block_on(async {
                        harness
                            .run(&control, |control| control.set_gain_db(GainDb::DEFAULT))
                            .await
                            .expect("manual unity target");
                        let _ = harness.render(WARM_FRAMES).await;
                        prepare_window(&harness).await;
                    });
                },
                |()| {
                    last = rt.block_on(async {
                        let start = harness.host().position() + 529;
                        let knots = vec![
                            ParameterKnot::new(
                                SessionFrame::new(i64::try_from(start).expect("session frame")),
                                0.0,
                                CurveShape::cubic(0.0, 1.0).expect("S shape"),
                            )
                            .expect("first knot"),
                            ParameterKnot::new(
                                SessionFrame::new(
                                    i64::try_from(start + 128).expect("session frame"),
                                ),
                                -12.0,
                                CurveShape::HOLD,
                            )
                            .expect("final knot"),
                        ];
                        harness
                            .run(&control, move |control| control.gain_program(knots))
                            .await
                            .expect("reachable curve");
                        harness.render(1024).await
                    });
                    black_box(&last);
                },
                BatchSize::PerIteration,
            );
            assert_eq!(last.len(), 2048);
            for (at, channels) in last.as_chunks::<2>().0.iter().enumerate() {
                let db = if at < 529 {
                    0.0
                } else if at >= 657 {
                    -12.0
                } else {
                    let time = (at - 529).to_f64().expect("local frame") / 128.0;
                    -12.0 * (3.0 * time * time - 2.0 * time * time * time)
                };
                let db = db.to_f32().expect("EQ range");
                assert_eq!(*channels, [0.5 * 10.0_f32.powf(db / 20.0); 2], "frame {at}");
            }
            assert_eq!(control.gain_db(), GainDb::from(-12.0));
        });
        group.finish();
    }
    rt.block_on(harness.close());
}
