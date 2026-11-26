use std::num::{NonZeroU32, NonZeroUsize};

use criterion::{BenchmarkId, Criterion, Throughput, measurement::Measurement};
use kithara_bufpool::{OverallBudget, PoolConfig, PoolRegion, pool_schema};
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

pool_schema! {
    pub(crate) BenchPools {
        samples: f32,
    }
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
    const STEADY: &[(&str, f64)] = &[("up", 1.25), ("down", 0.8)];

    let mut group = c.benchmark_group("resampler_glide_paths");
    group.throughput(Throughput::Elements(BLOCK.to_u64().unwrap_or(u64::MAX)));
    let input = input_buffers(CHANNELS, BLOCK);
    for &(name, ratio) in STEADY {
        let mut resampler = build_glide(ratio, BLOCK);
        let mut output = output_buffers(CHANNELS, resampler.output_frames_next());
        group.bench_function(format!("steady/{name}"), |b| {
            b.iter(|| {
                let process = process_once(&mut resampler, CHANNELS, &input, &mut output);
                std::hint::black_box(process);
            });
        });
    }
    let mut resampler = build_glide(1.0, BLOCK);
    let mut output = output_buffers(CHANNELS, EXACT_OUTPUT);
    group.bench_function("exact_span/exact", |b| {
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
        format!("r{source_rate}_{target_rate}_c{channels}_f{block}"),
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

fn process_once<R: Resampler + ?Sized>(
    resampler: &mut R,
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

fn criterion_benchmarks(c: &mut Criterion) {
    resampler_backends(c);
    resampler_glide_paths(c);
}

struct NativePrepared {
    resampler: Box<dyn Resampler>,
    input: Vec<Vec<f32>>,
    output: Vec<Vec<f32>>,
    channels: usize,
}

fn native_parameters(index: usize) -> (u32, u32, usize, usize) {
    const RATIOS: [(u32, u32); 4] = [
        (44_100, 48_000),
        (48_000, 44_100),
        (44_100, 22_050),
        (48_000, 48_000),
    ];
    let (source_rate, target_rate) = RATIOS[index / 4];
    let channels = [1, 2][(index / 2) % 2];
    let block = [1_024, 4_096][index % 2];
    (source_rate, target_rate, channels, block)
}

fn native_prepare<B: ResamplerBackend>(index: usize, backend: B) -> NativePrepared {
    let (source_rate, target_rate, channels, block) = native_parameters(index);
    let resampler = build_resampler(backend, source_rate, target_rate, channels, block);
    let output = output_buffers(channels, resampler.output_frames_next());
    NativePrepared {
        resampler: Box::new(resampler),
        input: input_buffers(channels, block),
        output,
        channels,
    }
}

fn native_process(mut prepared: NativePrepared) -> NativePrepared {
    prepared.resampler.reset();
    let process = process_once(
        &mut *prepared.resampler,
        prepared.channels,
        &prepared.input,
        &mut prepared.output,
    );
    std::hint::black_box(process);
    prepared
}

#[cfg(feature = "resample-rubato")]
fn native_rubato_async_prepare(index: usize) -> NativePrepared {
    native_prepare(
        index,
        RubatoBackend::with_config(
            RubatoConfig::builder()
                .algorithm(RubatoAlgorithm::Async)
                .build(),
        ),
    )
}

#[cfg(feature = "resample-rubato")]
#[metabench::benchmark(RUBATO_ASYNC, "resampler_backends", "rubato-async", gungraun_setup = native_rubato_async_prepare)]
#[bench::r44100_48000_c1_f1024(0_usize)]
#[bench::r44100_48000_c1_f4096(1_usize)]
#[bench::r44100_48000_c2_f1024(2_usize)]
#[bench::r44100_48000_c2_f4096(3_usize)]
#[bench::r48000_44100_c1_f1024(4_usize)]
#[bench::r48000_44100_c1_f4096(5_usize)]
#[bench::r48000_44100_c2_f1024(6_usize)]
#[bench::r48000_44100_c2_f4096(7_usize)]
#[bench::r44100_22050_c1_f1024(8_usize)]
#[bench::r44100_22050_c1_f4096(9_usize)]
#[bench::r44100_22050_c2_f1024(10_usize)]
#[bench::r44100_22050_c2_f4096(11_usize)]
#[bench::r48000_48000_c1_f1024(12_usize)]
#[bench::r48000_48000_c1_f4096(13_usize)]
#[bench::r48000_48000_c2_f1024(14_usize)]
#[bench::r48000_48000_c2_f4096(15_usize)]
fn native_rubato_async(prepared: NativePrepared) -> NativePrepared {
    native_process(prepared)
}

#[cfg(feature = "resample-rubato")]
fn native_rubato_fft_prepare(index: usize) -> NativePrepared {
    native_prepare(
        index,
        RubatoBackend::with_config(
            RubatoConfig::builder()
                .algorithm(RubatoAlgorithm::Fft)
                .build(),
        ),
    )
}

#[cfg(feature = "resample-rubato")]
#[metabench::benchmark(RUBATO_FFT, "resampler_backends", "rubato/config-fft", gungraun_setup = native_rubato_fft_prepare)]
#[bench::r44100_48000_c1_f1024(0_usize)]
#[bench::r44100_48000_c1_f4096(1_usize)]
#[bench::r44100_48000_c2_f1024(2_usize)]
#[bench::r44100_48000_c2_f4096(3_usize)]
#[bench::r48000_44100_c1_f1024(4_usize)]
#[bench::r48000_44100_c1_f4096(5_usize)]
#[bench::r48000_44100_c2_f1024(6_usize)]
#[bench::r48000_44100_c2_f4096(7_usize)]
#[bench::r44100_22050_c1_f1024(8_usize)]
#[bench::r44100_22050_c1_f4096(9_usize)]
#[bench::r44100_22050_c2_f1024(10_usize)]
#[bench::r44100_22050_c2_f4096(11_usize)]
#[bench::r48000_48000_c1_f1024(12_usize)]
#[bench::r48000_48000_c1_f4096(13_usize)]
#[bench::r48000_48000_c2_f1024(14_usize)]
#[bench::r48000_48000_c2_f4096(15_usize)]
fn native_rubato_fft(prepared: NativePrepared) -> NativePrepared {
    native_process(prepared)
}

#[cfg(feature = "resample-glide")]
fn native_glide_prepare(index: usize) -> NativePrepared {
    native_prepare(index, GlideBackend::new())
}

#[cfg(feature = "resample-glide")]
#[metabench::benchmark(GLIDE, "resampler_backends", "glide", gungraun_setup = native_glide_prepare)]
#[bench::r44100_48000_c1_f1024(0_usize)]
#[bench::r44100_48000_c1_f4096(1_usize)]
#[bench::r44100_48000_c2_f1024(2_usize)]
#[bench::r44100_48000_c2_f4096(3_usize)]
#[bench::r48000_44100_c1_f1024(4_usize)]
#[bench::r48000_44100_c1_f4096(5_usize)]
#[bench::r48000_44100_c2_f1024(6_usize)]
#[bench::r48000_44100_c2_f4096(7_usize)]
#[bench::r44100_22050_c1_f1024(8_usize)]
#[bench::r44100_22050_c1_f4096(9_usize)]
#[bench::r44100_22050_c2_f1024(10_usize)]
#[bench::r44100_22050_c2_f4096(11_usize)]
#[bench::r48000_48000_c1_f1024(12_usize)]
#[bench::r48000_48000_c1_f4096(13_usize)]
#[bench::r48000_48000_c2_f1024(14_usize)]
#[bench::r48000_48000_c2_f4096(15_usize)]
fn native_glide(prepared: NativePrepared) -> NativePrepared {
    native_process(prepared)
}

#[cfg(any(target_os = "macos", target_os = "ios"))]
fn native_apple_prepare(index: usize) -> NativePrepared {
    native_prepare(index, AppleAudioConverterBackend::new())
}

#[cfg(any(target_os = "macos", target_os = "ios"))]
#[metabench::benchmark(APPLE_AUDIO_CONVERTER, "resampler_backends", "apple-audio-converter", gungraun_setup = native_apple_prepare)]
#[bench::r44100_48000_c1_f1024(0_usize)]
#[bench::r44100_48000_c1_f4096(1_usize)]
#[bench::r44100_48000_c2_f1024(2_usize)]
#[bench::r44100_48000_c2_f4096(3_usize)]
#[bench::r48000_44100_c1_f1024(4_usize)]
#[bench::r48000_44100_c1_f4096(5_usize)]
#[bench::r48000_44100_c2_f1024(6_usize)]
#[bench::r48000_44100_c2_f4096(7_usize)]
#[bench::r44100_22050_c1_f1024(8_usize)]
#[bench::r44100_22050_c1_f4096(9_usize)]
#[bench::r44100_22050_c2_f1024(10_usize)]
#[bench::r44100_22050_c2_f4096(11_usize)]
#[bench::r48000_48000_c1_f1024(12_usize)]
#[bench::r48000_48000_c1_f4096(13_usize)]
#[bench::r48000_48000_c2_f1024(14_usize)]
#[bench::r48000_48000_c2_f4096(15_usize)]
#[cfg(any(target_os = "macos", target_os = "ios"))]
fn native_apple(prepared: NativePrepared) -> NativePrepared {
    native_process(prepared)
}

struct GlidePrepared {
    resampler: GlideResampler,
    input: Vec<Vec<f32>>,
    output: Vec<Vec<f32>>,
    exact: bool,
}

fn glide_prepare(index: usize) -> GlidePrepared {
    let (ratio, exact) = [(1.25, false), (0.8, false), (1.0, true)][index];
    let resampler = build_glide(ratio, 1_024);
    let output_frames = if exact {
        819
    } else {
        resampler.output_frames_next()
    };
    GlidePrepared {
        resampler,
        input: input_buffers(2, 1_024),
        output: output_buffers(2, output_frames),
        exact,
    }
}

fn glide_action(mut prepared: GlidePrepared) -> GlidePrepared {
    if prepared.exact {
        let (left, right) = prepared.output.split_at_mut(1);
        prepared
            .resampler
            .process_exact_span(
                &[&prepared.input[0][..], &prepared.input[1][..]],
                &mut [&mut left[0][..], &mut right[0][..]],
            )
            .unwrap_or_else(|err| panic!("exact span should render: {err}"));
    } else {
        let process = process_once(
            &mut prepared.resampler,
            2,
            &prepared.input,
            &mut prepared.output,
        );
        std::hint::black_box(process);
    }
    prepared
}

#[metabench::benchmark(GLIDE_STEADY, "resampler_glide_paths", "steady", gungraun_setup = glide_prepare)]
#[bench::up(0_usize)]
#[bench::down(1_usize)]
fn native_glide_steady(prepared: GlidePrepared) -> GlidePrepared {
    glide_action(prepared)
}

#[metabench::benchmark(GLIDE_EXACT, "resampler_glide_paths", "exact_span", gungraun_setup = glide_prepare)]
#[bench::exact(2_usize)]
fn native_glide_exact(prepared: GlidePrepared) -> GlidePrepared {
    glide_action(prepared)
}

#[cfg(any(target_os = "macos", target_os = "ios"))]
metabench::main!(
    criterion = criterion_benchmarks,
    benchmarks = [
        RUBATO_ASYNC,
        RUBATO_FFT,
        GLIDE,
        APPLE_AUDIO_CONVERTER,
        GLIDE_STEADY,
        GLIDE_EXACT
    ],
    allocator = std::alloc::System,
);

#[cfg(not(any(target_os = "macos", target_os = "ios")))]
metabench::main!(
    criterion = criterion_benchmarks,
    benchmarks = [RUBATO_ASYNC, RUBATO_FFT, GLIDE, GLIDE_STEADY, GLIDE_EXACT],
    allocator = std::alloc::System,
);
