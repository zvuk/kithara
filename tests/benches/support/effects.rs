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
    rt.block_on(harness.close());
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
