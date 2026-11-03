use std::{hint::black_box, num::NonZeroU32};

use criterion::{BatchSize, BenchmarkId, Criterion, Throughput};
use kithara::{
    effects::{
        AudioEffect, GainDb,
        eq::{EqBandConfig, EqConfig, EqEffect, EqLayout, StereoEq, generate_log_spaced_bands},
    },
    signal::{AudioChunk, AudioChunkInfo, AudioSpec},
};
use kithara_integration_tests::bufpool_ext::{pools, sample_buffer};

const RATE: NonZeroU32 = NonZeroU32::new(48_000).expect("bench sample rate");

fn band_configs(count: usize) -> Vec<EqBandConfig> {
    let mut bands = generate_log_spaced_bands(count);
    for (band, config) in bands.iter_mut().enumerate() {
        config.set_gain_db(GainDb::from(if band % 2 == 0 { -6.0 } else { 3.0 }));
    }
    bands
}

fn layout<S: kithara::bufpool::HasPool<f32>>(config: &EqConfig<S>, count: usize) -> Box<EqLayout> {
    Box::new(EqLayout::new(config, &band_configs(count), RATE).expect("prepared EQ layout"))
}

pub(super) fn bench(c: &mut Criterion) {
    let pools = pools();
    let config = EqConfig::builder(pools.clone()).build();
    let mut group = c.benchmark_group("eq_crossover");
    group.sample_size(20);
    for (bands, frames, label) in [
        (3, 1, "settled"),
        (3, 128, "settled"),
        (10, 257, "settled"),
        (3, 128, "moving"),
        (10, 257, "moving"),
        (10, 257, "quiet"),
        (10, 257, "pathological"),
    ] {
        let mut eq =
            StereoEq::with_layout(&config, RATE, layout(&config, bands)).expect("valid EQ layout");
        let source: Vec<_> = (0..frames)
            .map(|frame| match label {
                "quiet" => f32::MIN_POSITIVE,
                "pathological" => [f32::MAX, f32::NAN, f32::INFINITY, f32::from_bits(1)][frame % 4],
                _ => f32::from(u16::try_from(frame % 127).expect("bounded phase")) / 127.0 - 0.5,
            })
            .collect();
        let mut left = source.clone();
        let mut right = source.clone();
        let mut up = false;
        group.throughput(Throughput::Elements(
            u64::try_from(frames).expect("frames fit u64"),
        ));
        group.bench_with_input(
            BenchmarkId::new(format!("{label}_{bands}band"), frames),
            &frames,
            |b, _| {
                b.iter(|| {
                    left.copy_from_slice(black_box(&source));
                    right.copy_from_slice(black_box(&source));
                    if label == "moving" {
                        up = !up;
                        eq.set_gain(0, GainDb::from(if up { 3.0 } else { -6.0 }));
                    }
                    eq.process(black_box(&mut left), black_box(&mut right));
                    black_box((&left, &right));
                });
            },
        );
    }
    group.throughput(Throughput::Elements(128));
    group.bench_function("layout_3to10/128", |b| {
        b.iter_batched_ref(
            || {
                (
                    StereoEq::with_layout(&config, RATE, layout(&config, 3))
                        .expect("valid EQ layout"),
                    Some(layout(&config, 10)),
                    [0.25; 128],
                    [-0.125; 128],
                )
            },
            |(eq, incoming, left, right)| {
                eq.take_layout(incoming.take().expect("one prepared layout per iteration"))
                    .expect("valid layout retune");
                eq.process(black_box(left), black_box(right));
                black_box((left, right));
            },
            BatchSize::PerIteration,
        );
    });
    group.throughput(Throughput::Elements(1));
    group.bench_function("retiring_3to10/1", |b| {
        b.iter_batched_ref(
            || {
                let mut eq = StereoEq::with_layout(&config, RATE, layout(&config, 3))
                    .expect("valid EQ layout");
                eq.take_layout(layout(&config, 10))
                    .expect("valid layout retune");
                eq.process(&mut [0.25], &mut [-0.125]);
                (eq, [0.25], [-0.125])
            },
            |(eq, left, right)| {
                eq.process(black_box(left), black_box(right));
                black_box((left, right));
            },
            BatchSize::PerIteration,
        );
    });
    group.throughput(Throughput::Elements(7));
    let mut effect =
        EqEffect::new(&config, band_configs(3), RATE.get(), 2).expect("prepared interleaved EQ");
    let info = AudioChunkInfo {
        spec: AudioSpec::new(2, RATE),
        frames: 7,
        ..Default::default()
    };
    let prepare = || AudioChunk::new(info, sample_buffer(&pools, &[0.25, -0.125].repeat(7)));
    let probe = effect.process(prepare()).expect("processed EQ probe");
    assert_eq!(probe.meta, info);
    assert_eq!(probe.meta.frames, 7);
    assert_eq!(probe.samples.len(), 14);
    drop(probe);
    group.bench_function("effect_3band/7", |b| {
        b.iter_batched(
            prepare,
            |chunk| black_box(effect.process(black_box(chunk)).expect("processed EQ")),
            BatchSize::PerIteration,
        );
    });
    group.finish();
}
