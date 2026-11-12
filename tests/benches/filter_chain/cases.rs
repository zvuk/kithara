use std::{hint::black_box, num::NonZeroU32};

use criterion::{BatchSize, Criterion};
use kithara::{
    bufpool::SampleBuffer,
    effects::filter::{FilterEffect, FilterKnob, FilterSettings},
    signal::AudioSpec,
};
use kithara_test_utils::bufpool::{Pools, pools, sample_buffer};

/// The deck filter held in its low-pass half, where every frame is filtered.
fn prepared_filter(spec: AudioSpec) -> FilterEffect {
    let mut filter = FilterEffect::new(spec, FilterSettings::default()).expect("prepared filter");
    filter.configure(FilterKnob::checked(-0.5).expect("low-pass knob"));
    filter.reset();
    filter
}

fn input_planes(pools: &Pools, spec: AudioSpec, input: &[f32]) -> Vec<SampleBuffer> {
    let channels = usize::from(spec.channels);
    (0..channels)
        .map(|channel| {
            sample_buffer(
                pools,
                &input
                    .iter()
                    .skip(channel)
                    .step_by(channels)
                    .copied()
                    .collect::<Vec<_>>(),
            )
        })
        .collect()
}

pub(crate) fn bench(c: &mut Criterion) {
    let pools = pools();
    let rate = NonZeroU32::new(48_000).expect("bench rate");
    let mut group = c.benchmark_group("filter_chain");
    for (name, channels, frames) in [
        ("lowpass_stereo_empty", 2, 0),
        ("lowpass_mono_1", 1, 1),
        ("lowpass_stereo_7", 2, 7),
        ("lowpass_stereo_128", 2, 128),
        ("lowpass_stereo_257", 2, 257),
        ("lowpass_surround_257", 6, 257),
    ] {
        let spec = AudioSpec::new(channels, rate);
        let fixture = crate::make_pcm(frames, usize::from(channels));
        let mut filter = prepared_filter(spec);
        group.bench_function(name, |b| {
            b.iter_batched_ref(
                || input_planes(&pools, spec, &fixture),
                |planes| {
                    filter
                        .process(black_box(planes), 0..frames)
                        .expect("admitted planes");
                },
                BatchSize::LargeInput,
            );
        });
    }
    let spec = AudioSpec::new(2, rate);
    let mut filter = prepared_filter(spec);
    let mut burst = vec![0.0; 514];
    burst[..64].fill(0.25);
    let silence = vec![0.0; 514];
    group.bench_function("lowpass_burst_and_two_quiet_blocks_stereo_257", |b| {
        b.iter_batched_ref(
            || {
                [
                    input_planes(&pools, spec, &burst),
                    input_planes(&pools, spec, &silence),
                    input_planes(&pools, spec, &silence),
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
