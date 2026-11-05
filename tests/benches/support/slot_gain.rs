use std::{
    hint::black_box,
    num::{NonZeroU32, NonZeroUsize},
};

use criterion::{BatchSize, Criterion};
use kithara::{
    assets::AssetStore,
    audio::mock::TestPcmReader,
    play::{CurveShape, ParameterKnot, ResourceConfig, ResourceSrc, SlotGainProgram},
    queue::TrackSource,
    signal::{AudioSpec, SessionFrame},
    warp::WarpConfig,
};
use kithara_integration_tests::offline::{OfflinePlayer, OfflinePlayerOptions};
use kithara_test_fixtures::integration_fixtures::benchmark_half;

const RATE: u32 = 44_100;
const WARM_FRAMES: usize = 4096;

async fn loaded_player() -> OfflinePlayer {
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
    let spec = AudioSpec::new(2, NonZeroU32::new(RATE).expect("nonzero fixture rate"));
    let TrackSource::Uri(uri) = harness.pcm_deck(Box::new(TestPcmReader::with_pcm(
        spec,
        600.0,
        benchmark_half(),
    ))) else {
        panic!("prepared PCM fixture is a URI")
    };
    let config = ResourceConfig::for_src(ResourceSrc::parse(uri).expect("fixture URI parses"))
        .store(AssetStore::builder(harness.worker().pools().clone()).build())
        .preload_chunks(NonZeroUsize::MIN)
        .audio_buffer_chunks(NonZeroUsize::new(2).expect("nonzero buffer chunks"))
        .build();
    harness.load_config(config).await;
    let _ = harness.render(WARM_FRAMES).await;
    harness
}

async fn gain_window(
    harness: &OfflinePlayer,
    item: kithara_events::TrackId,
    target: f64,
) -> Vec<f32> {
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

pub(super) fn host_curve(c: &mut Criterion) {
    let rt = super::make_runtime();
    let harness = rt.block_on(loaded_player());
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
    let mut last = Vec::new();
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

pub(super) fn gain_shape(c: &mut Criterion) {
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
