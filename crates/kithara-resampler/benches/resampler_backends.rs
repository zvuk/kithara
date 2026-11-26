use std::num::{NonZeroU32, NonZeroUsize};

use criterion::{
    BenchmarkId, Criterion, Throughput, criterion_group, criterion_main, measurement::Measurement,
};
use kithara_bufpool::{OverallBudget, Pool, PoolConfig, PoolRegion, PoolSlot};
#[cfg(any(target_os = "macos", target_os = "ios"))]
use kithara_resampler::apple::AppleAudioConverterBackend;
#[cfg(feature = "resample-glide")]
use kithara_resampler::glide::{GlideBackend, GlideResampler};
#[cfg(feature = "resample-rubato")]
use kithara_resampler::rubato::{RubatoAlgorithm, RubatoBackend, RubatoConfig};
use kithara_resampler::{
    Resampler, ResamplerBackend, ResamplerConfig, ResamplerMode, ResamplerOptions,
    ResamplerSettings, create_resampler,
};
use num_traits::cast::ToPrimitive;

#[derive(Pool)]
pub(crate) struct BenchPools {
    samples: PoolSlot<f32>,
}

fn pools() -> PoolRegion<BenchPools> {
    BenchPools::builder(OverallBudget(64 * 1024 * 1024))
        .samples(PoolConfig::builder().max_buffers(64).build())
        .build()
        .unwrap_or_else(|error| panic!("bench pool region: {error}"))
}

fn resampler_backends(c: &mut Criterion) {
    const RATIOS: &[(u32, u32)] = &[
        (44_100, 48_000),
        (48_000, 44_100),
        (44_100, 22_050),
        (48_000, 48_000),
    ];

    const CHANNELS: &[usize] = &[1, 2];
    const BLOCKS: &[usize] = &[1_024, 4_096];

    let mut group = c.benchmark_group("resampler_backends");
    for &(source_rate, target_rate) in RATIOS {
        for &channels in CHANNELS {
            for &block in BLOCKS {
                group.throughput(Throughput::Elements(block.to_u64().unwrap_or(u64::MAX)));
                bench_compiled_backends(&mut group, source_rate, target_rate, channels, block);
            }
        }
    }
    group.finish();
}

#[cfg(feature = "resample-glide")]
fn resampler_glide_paths(c: &mut Criterion) {
    const BLOCK: usize = 1_024;
    const CHANNELS: usize = 2;
    const EXACT_OUTPUT: usize = 819;
    const STEADY: &[(&str, f64)] = &[("steady_up", 1.25), ("steady_down", 0.8)];

    let mut group = c.benchmark_group("resampler_glide_paths");
    group.throughput(Throughput::Elements(BLOCK.to_u64().unwrap_or(u64::MAX)));
    let input = input_buffers(CHANNELS, BLOCK);
    for &(name, ratio) in STEADY {
        let mut resampler = build_glide(ratio, BLOCK);
        let mut output = output_buffers(CHANNELS, resampler.output_frames_next());
        group.bench_function(name, |b| {
            b.iter(|| {
                let process = process_once(&mut resampler, CHANNELS, &input, &mut output);
                std::hint::black_box(process);
            });
        });
    }
    let mut resampler = build_glide(1.0, BLOCK);
    let mut output = output_buffers(CHANNELS, EXACT_OUTPUT);
    group.bench_function("exact_span", |b| {
        b.iter(|| {
            let (left, right) = output.split_at_mut(1);
            resampler
                .process_exact_span(
                    &[&input[0][..], &input[1][..]],
                    &mut [&mut left[0][..], &mut right[0][..]],
                )
                .unwrap_or_else(|err| panic!("exact span should render: {err}"));
        });
    });
    group.finish();
}

fn bench_compiled_backends<M>(
    group: &mut criterion::BenchmarkGroup<'_, M>,
    source_rate: u32,
    target_rate: u32,
    channels: usize,
    block: usize,
) where
    M: Measurement,
{
    #[cfg(feature = "resample-rubato")]
    bench_backend(
        group,
        "rubato-async",
        RubatoBackend::with_config(
            RubatoConfig::builder()
                .algorithm(RubatoAlgorithm::Async)
                .build(),
        ),
        source_rate,
        target_rate,
        channels,
        block,
    );
    #[cfg(feature = "resample-rubato")]
    bench_backend(
        group,
        "rubato/config-fft",
        RubatoBackend::with_config(
            RubatoConfig::builder()
                .algorithm(RubatoAlgorithm::Fft)
                .build(),
        ),
        source_rate,
        target_rate,
        channels,
        block,
    );
    #[cfg(feature = "resample-glide")]
    bench_backend(
        group,
        "glide",
        GlideBackend::new(),
        source_rate,
        target_rate,
        channels,
        block,
    );
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    bench_backend(
        group,
        "apple-audio-converter",
        AppleAudioConverterBackend::new(),
        source_rate,
        target_rate,
        channels,
        block,
    );
}

fn bench_backend<M, B>(
    group: &mut criterion::BenchmarkGroup<'_, M>,
    backend_name: &'static str,
    backend: B,
    source_rate: u32,
    target_rate: u32,
    channels: usize,
    block: usize,
) where
    M: Measurement,
    B: ResamplerBackend,
{
    let mut resampler = build_resampler(backend, source_rate, target_rate, channels, block);
    let input = input_buffers(channels, block);
    let mut output = output_buffers(channels, resampler.output_frames_next());
    let id = BenchmarkId::new(
        backend_name,
        format!("{source_rate}-{target_rate}/{channels}ch/{block}f"),
    );
    group.bench_with_input(id, &block, |b, _| {
        b.iter(|| {
            resampler.reset();
            let process = process_once(&mut resampler, channels, &input, &mut output);
            std::hint::black_box(process);
        });
    });
}

fn build_resampler<B>(
    backend: B,
    source_rate: u32,
    target_rate: u32,
    channels: usize,
    block: usize,
) -> B::Resampler
where
    B: ResamplerBackend,
{
    let settings = ResamplerSettings::builder()
        .channels(non_zero_usize(channels))
        .mode(ResamplerMode::FixedRatio {
            source_sample_rate: non_zero_u32(source_rate),
            target_sample_rate: non_zero_u32(target_rate),
        })
        .options(ResamplerOptions::builder().chunk_size(block).build())
        .pools(pools())
        .build();
    let config = ResamplerConfig::builder()
        .backend(backend)
        .settings(settings)
        .build();
    create_resampler(&config).unwrap_or_else(|err| panic!("resampler should build: {err}"))
}

#[cfg(feature = "resample-glide")]
fn build_glide(initial_ratio: f64, block: usize) -> GlideResampler {
    let settings = ResamplerSettings::builder()
        .channels(non_zero_usize(2))
        .mode(ResamplerMode::VariableRatio {
            sample_rate: non_zero_u32(48_000),
            initial_ratio,
            glide: None,
        })
        .options(ResamplerOptions::builder().chunk_size(block).build())
        .pools(pools())
        .build();
    GlideBackend::new()
        .build(&settings)
        .unwrap_or_else(|err| panic!("glide should build: {err}"))
}

fn input_buffers(channels: usize, frames: usize) -> Vec<Vec<f32>> {
    (0..channels)
        .map(|channel| {
            (0..frames)
                .map(|frame| {
                    let frame = frame.to_f32().unwrap_or(0.0);
                    let channel = channel.to_f32().unwrap_or(0.0);
                    (frame.mul_add(0.017, channel * 0.13)).sin()
                })
                .collect()
        })
        .collect()
}

const fn non_zero_u32(value: u32) -> NonZeroU32 {
    NonZeroU32::new(value).expect("value must be non-zero")
}

const fn non_zero_usize(value: usize) -> NonZeroUsize {
    NonZeroUsize::new(value).expect("value must be non-zero")
}

fn output_buffers(channels: usize, frames: usize) -> Vec<Vec<f32>> {
    (0..channels).map(|_| vec![0.0; frames]).collect()
}

fn process_once(
    resampler: &mut impl Resampler,
    channels: usize,
    input: &[Vec<f32>],
    output: &mut [Vec<f32>],
) -> kithara_resampler::ResamplerProcess {
    match channels {
        1 => {
            let input_refs = [&input[0][..]];
            let mut output_refs = [&mut output[0][..]];
            resampler
                .process_into_buffer(&input_refs, &mut output_refs)
                .unwrap_or_else(|err| panic!("resampler process should succeed: {err}"))
        }
        2 => {
            let input_refs = [&input[0][..], &input[1][..]];
            let (left, right) = output.split_at_mut(1);
            let mut output_refs = [&mut left[0][..], &mut right[0][..]];
            resampler
                .process_into_buffer(&input_refs, &mut output_refs)
                .unwrap_or_else(|err| panic!("resampler process should succeed: {err}"))
        }
        _ => panic!("bench supports mono and stereo"),
    }
}

#[cfg(feature = "resample-glide")]
criterion_group!(benches, resampler_backends, resampler_glide_paths);
#[cfg(not(feature = "resample-glide"))]
criterion_group!(benches, resampler_backends);
criterion_main!(benches);
