use std::{hint::black_box, num::NonZeroU32};

use criterion::{BatchSize, Criterion};
use kithara::{
    bufpool::SampleBuffer,
    effects::filter::{FilterEffect, FilterKnob, FilterSettings},
};
use kithara_test_utils::bufpool::{Pools, pools, sample_buffer};

/// The deck filter held in its low-pass half, where every frame is filtered.
fn prepared_filter(rate: NonZeroU32) -> FilterEffect {
    let mut filter = FilterEffect::new(rate, FilterSettings::default());
    filter.configure(FilterKnob::checked(-0.5).expect("low-pass knob"));
    filter.reset();
    filter
}

/// The stereo planes of interleaved `input`.
fn input_planes(pools: &Pools, input: &[f32]) -> [SampleBuffer; 2] {
    [0, 1].map(|channel| {
        sample_buffer(
            pools,
            &input
                .iter()
                .skip(channel)
                .step_by(2)
                .copied()
                .collect::<Vec<_>>(),
        )
    })
}

pub(crate) fn bench(c: &mut Criterion) {
    let pools = pools();
    let rate = NonZeroU32::new(48_000).expect("bench rate");
    let mut group = c.benchmark_group("filter_chain");
    for (name, frames) in [
        ("lowpass_stereo_empty", 0),
        ("lowpass_stereo_7", 7),
        ("lowpass_stereo_128", 128),
        ("lowpass_stereo_257", 257),
    ] {
        let fixture = crate::make_pcm(frames, 2);
        let mut filter = prepared_filter(rate);
        group.bench_function(name, |b| {
            b.iter_batched_ref(
                || input_planes(&pools, &fixture),
                |planes| {
                    filter
                        .process(black_box(planes), 0..frames)
                        .expect("admitted planes");
                },
                BatchSize::LargeInput,
            );
        });
    }
    let mut filter = prepared_filter(rate);
    let mut burst = vec![0.0; 514];
    burst[..64].fill(0.25);
    let silence = vec![0.0; 514];
    group.bench_function("lowpass_burst_and_two_quiet_blocks_stereo_257", |b| {
        b.iter_batched_ref(
            || {
                [
                    input_planes(&pools, &burst),
                    input_planes(&pools, &silence),
                    input_planes(&pools, &silence),
                ]
            },
            |inputs| {
                for planes in inputs {
                    filter
                        .process(black_box(planes), 0..257)
                        .expect("admitted planes");
                }
            },
            BatchSize::LargeInput,
        );
    });
    group.finish();
}
