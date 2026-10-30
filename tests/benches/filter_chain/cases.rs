use std::{hint::black_box, num::NonZeroU32};

use criterion::{BatchSize, Criterion};
use kithara::{
    effects::{
        AudioEffect, apply_effects,
        filter::{Cutoff, FilterConfig, FilterEffect, Kind},
    },
    signal::{AudioChunk, AudioChunkInfo, AudioSpec},
};
use kithara_test_utils::bufpool::{Pools, pools, sample_buffer};

fn prepared_chain(pools: &Pools, spec: AudioSpec) -> Vec<Box<dyn AudioEffect>> {
    let config = FilterConfig::builder(pools.clone())
        .kind(Kind::LowPass)
        .cutoff_hz(Cutoff::checked(1_000.0).expect("admitted cutoff"))
        .build()
        .expect("admitted settings");
    vec![Box::new(
        FilterEffect::new(spec, config).expect("prepared filter"),
    )]
}

fn input_chunk(pools: &Pools, spec: AudioSpec, input: &[f32]) -> AudioChunk {
    AudioChunk::new(
        AudioChunkInfo {
            spec,
            frames: u32::try_from(input.len() / usize::from(spec.channels))
                .expect("bounded frames"),
            ..Default::default()
        },
        sample_buffer(pools, input),
    )
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
        let mut chain = prepared_chain(&pools, spec);
        group.bench_function(name, |b| {
            b.iter_batched_ref(
                || Some(input_chunk(&pools, spec, &fixture)),
                |input| {
                    apply_effects(&mut chain, black_box(input.take().expect("prepared PCM")))
                        .expect("admitted format")
                        .expect("causal filter output")
                },
                BatchSize::LargeInput,
            );
        });
    }
    let spec = AudioSpec::new(2, rate);
    let mut chain = prepared_chain(&pools, spec);
    let mut burst = vec![0.0; 514];
    burst[..64].fill(0.25);
    let silence = vec![0.0; 514];
    group.bench_function("lowpass_burst_and_two_quiet_blocks_stereo_257", |b| {
        b.iter_batched_ref(
            || {
                [
                    Some(input_chunk(&pools, spec, &burst)),
                    Some(input_chunk(&pools, spec, &silence)),
                    Some(input_chunk(&pools, spec, &silence)),
                ]
            },
            |inputs| {
                inputs.each_mut().map(|input| {
                    apply_effects(&mut chain, black_box(input.take().expect("prepared PCM")))
                        .expect("admitted format")
                        .expect("causal filter output")
                })
            },
            BatchSize::LargeInput,
        );
    });
    group.finish();
}
