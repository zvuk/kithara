use std::{hint::black_box, num::NonZeroU32};

use criterion::{BatchSize, BenchmarkId, Criterion, Throughput};
use kithara::effects::{
    GainDb,
    eq::{EqConfig, EqLayout, StereoEq, generate_log_spaced_bands},
};
use kithara_integration_tests::bufpool_ext::pools;

const RATE: NonZeroU32 = NonZeroU32::new(48_000).expect("bench sample rate");

fn layout<S: kithara::bufpool::HasPool<f32>>(config: &EqConfig<S>, count: usize) -> Box<EqLayout> {
    let mut bands = generate_log_spaced_bands(count);
    for (band, config) in bands.iter_mut().enumerate() {
        config.set_gain_db(GainDb::from(if band % 2 == 0 { -6.0 } else { 3.0 }));
    }
    Box::new(EqLayout::new(config, &bands, RATE).expect("prepared EQ layout"))
}

pub(super) fn bench(c: &mut Criterion) {
    let config = EqConfig::builder(pools()).build();
    let mut group = c.benchmark_group("eq_crossover");
    group.sample_size(20);
    for (bands, frames, label) in [
        (3, 128, "settled"),
        (10, 257, "settled"),
        (3, 128, "moving"),
        (10, 257, "moving"),
        (10, 257, "quiet"),
        (10, 257, "pathological"),
    ] {
        let mut eq = StereoEq::with_layout(&config, RATE, layout(&config, bands));
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
                    StereoEq::with_layout(&config, RATE, layout(&config, 3)),
                    Some(layout(&config, 10)),
                    [0.25; 128],
                    [-0.125; 128],
                )
            },
            |(eq, incoming, left, right)| {
                eq.take_layout(incoming.take().expect("one prepared layout per iteration"));
                eq.process(black_box(left), black_box(right));
                black_box((left, right));
            },
            BatchSize::PerIteration,
        );
    });
    group.finish();
}
