#[cfg(all(feature = "apple", any(target_os = "macos", target_os = "ios")))]
mod apple {
    use std::num::{NonZeroU32, NonZeroUsize};

    use criterion::{BenchmarkId, Criterion, Throughput};
    use kithara_bufpool::PcmPool;
    use kithara_decode::AppleAudioConverterBackend;
    use kithara_resampler::{
        Resampler, ResamplerConfig, ResamplerMode, ResamplerOptions, ResamplerSettings,
        create_resampler,
    };
    use num_traits::cast::ToPrimitive;

    const RATIOS: &[(u32, u32)] = &[
        (44_100, 48_000),
        (48_000, 44_100),
        (44_100, 22_050),
        (48_000, 48_000),
    ];
    const CHANNELS: &[usize] = &[1, 2];
    const BLOCKS: &[usize] = &[1_024, 4_096];

    pub(super) fn main() {
        let mut criterion = Criterion::default().configure_from_args();
        apple_resampler(&mut criterion);
        criterion.final_summary();
    }

    fn apple_resampler(c: &mut Criterion) {
        let mut group = c.benchmark_group("apple_resampler");
        for &(source_rate, target_rate) in RATIOS {
            for &channels in CHANNELS {
                for &block in BLOCKS {
                    group.throughput(Throughput::Elements(block.to_u64().unwrap_or(u64::MAX)));
                    bench_backend(&mut group, source_rate, target_rate, channels, block);
                }
            }
        }
        group.finish();
    }

    fn bench_backend<M>(
        group: &mut criterion::BenchmarkGroup<'_, M>,
        source_rate: u32,
        target_rate: u32,
        channels: usize,
        block: usize,
    ) where
        M: criterion::measurement::Measurement,
    {
        let mut resampler = build_resampler(source_rate, target_rate, channels, block);
        let input = input_buffers(channels, block);
        let mut output = output_buffers(channels, resampler.output_frames_next());
        let id = BenchmarkId::new(
            "apple-audio-converter",
            format!("{source_rate}-{target_rate}/{channels}ch/{block}f"),
        );
        group.bench_with_input(id, &block, |b, _| {
            b.iter(|| {
                resampler.reset();
                let process = process_once(&mut *resampler, channels, &input, &mut output);
                std::hint::black_box(process);
            });
        });
    }

    fn build_resampler(
        source_rate: u32,
        target_rate: u32,
        channels: usize,
        block: usize,
    ) -> Box<dyn Resampler> {
        let settings = ResamplerSettings::builder()
            .channels(non_zero_usize(channels))
            .mode(ResamplerMode::FixedRatio {
                source_sample_rate: non_zero_u32(source_rate),
                target_sample_rate: non_zero_u32(target_rate),
            })
            .options(ResamplerOptions::builder().chunk_size(block).build())
            .pcm_pool(PcmPool::new(
                64,
                block.saturating_mul(channels).saturating_mul(4),
            ))
            .build();
        let config = ResamplerConfig::builder()
            .backend(AppleAudioConverterBackend::new())
            .settings(settings)
            .build();
        create_resampler(&config)
            .unwrap_or_else(|err| panic!("Apple resampler should build: {err}"))
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

    fn non_zero_u32(value: u32) -> NonZeroU32 {
        NonZeroU32::new(value).unwrap_or_else(|| panic!("value must be non-zero"))
    }

    fn non_zero_usize(value: usize) -> NonZeroUsize {
        NonZeroUsize::new(value).unwrap_or_else(|| panic!("value must be non-zero"))
    }

    fn output_buffers(channels: usize, frames: usize) -> Vec<Vec<f32>> {
        (0..channels).map(|_| vec![0.0; frames]).collect()
    }

    fn process_once(
        resampler: &mut dyn Resampler,
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
                    .unwrap_or_else(|err| panic!("Apple resampler process failed: {err}"))
            }
            2 => {
                let input_refs = [&input[0][..], &input[1][..]];
                let (left, right) = output.split_at_mut(1);
                let mut output_refs = [&mut left[0][..], &mut right[0][..]];
                resampler
                    .process_into_buffer(&input_refs, &mut output_refs)
                    .unwrap_or_else(|err| panic!("Apple resampler process failed: {err}"))
            }
            _ => panic!("bench supports mono and stereo"),
        }
    }
}

#[cfg(all(feature = "apple", any(target_os = "macos", target_os = "ios")))]
fn main() {
    apple::main();
}

#[cfg(not(all(feature = "apple", any(target_os = "macos", target_os = "ios"))))]
fn main() {}
