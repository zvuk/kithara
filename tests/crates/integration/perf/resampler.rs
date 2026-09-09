#![cfg(feature = "perf")]

use std::num::{NonZeroU32, NonZeroUsize};

use hotpath::HotpathGuardBuilder;
use kithara::{
    resampler::{
        Resampler, ResamplerConfig, ResamplerMode, ResamplerOptions, ResamplerQuality,
        ResamplerSettings, create_resampler, rubato::RubatoBackend,
    },
    signal::{AudioChunk, AudioChunkInfo, AudioSpec},
};
use kithara_integration_tests::bufpool_ext::pools;
use kithara_test_fixtures::integration_fixtures::{perf_interleaved, perf_planar};

/// Create a test PCM chunk with specified sample count.
fn create_test_chunk(frames: usize, spec: AudioSpec, input: &[f32]) -> AudioChunk {
    let samples = frames * spec.channels as usize;
    let pools = pools();
    let mut pcm = pools
        .get_with_len::<f32>(samples)
        .expect("perf sample buffer");
    pcm.copy_from_slice(&input[..samples]);

    AudioChunk::new(
        AudioChunkInfo {
            spec,
            ..Default::default()
        },
        pcm,
    )
}

fn create_planar(frames: usize, source: &[Vec<f32>; 2]) -> [Vec<f32>; 2] {
    [source[0][..frames].to_vec(), source[1][..frames].to_vec()]
}

fn create_output(resampler: &dyn Resampler) -> [Vec<f32>; 2] {
    [
        vec![0.0; resampler.output_frames_next()],
        vec![0.0; resampler.output_frames_next()],
    ]
}

fn build_resampler(
    source_rate: u32,
    target_rate: u32,
    quality: ResamplerQuality,
    chunk_size: usize,
) -> Box<dyn Resampler> {
    let settings = ResamplerSettings::builder()
        .channels(NonZeroUsize::new(2).unwrap_or_else(|| panic!("test channels")))
        .mode(ResamplerMode::FixedRatio {
            source_sample_rate: NonZeroU32::new(source_rate)
                .unwrap_or_else(|| panic!("test source rate")),
            target_sample_rate: NonZeroU32::new(target_rate)
                .unwrap_or_else(|| panic!("test target rate")),
        })
        .quality(quality)
        .options(ResamplerOptions::builder().chunk_size(chunk_size).build())
        .pools(pools())
        .build();
    let config = ResamplerConfig::builder()
        .backend(RubatoBackend::new())
        .settings(settings)
        .build();
    Box::new(
        create_resampler(&config).unwrap_or_else(|err| panic!("resampler should build: {err}")),
    )
}

fn process_stereo(
    resampler: &mut dyn Resampler,
    input: &[Vec<f32>; 2],
    output: &mut [Vec<f32>; 2],
) -> usize {
    let input_refs = [&input[0][..], &input[1][..]];
    let (left, right) = output.split_at_mut(1);
    let mut output_refs = [&mut left[0][..], &mut right[0][..]];
    resampler
        .process_into_buffer(&input_refs, &mut output_refs)
        .unwrap_or_else(|err| panic!("resampler process should succeed: {err}"))
        .output_frames
}

#[hotpath::measure]
fn resampler_process_single(
    resampler: &mut dyn Resampler,
    input: &[Vec<f32>; 2],
    output: &mut [Vec<f32>; 2],
) {
    let _ = process_stereo(resampler, input, output);
}

#[hotpath::measure]
fn resampler_passthrough_process(
    resampler: &mut dyn Resampler,
    input: &[Vec<f32>; 2],
    output: &mut [Vec<f32>; 2],
) {
    let _ = process_stereo(resampler, input, output);
}

/// Detailed breakdown of resampling operations for a single quality preset.
/// Measures: process overhead, actual resampling work, buffer management.
#[hotpath::measure]
fn resampler_full_process_cycle(
    resampler: &mut dyn Resampler,
    input: &[Vec<f32>; 2],
    output: &mut [Vec<f32>; 2],
) -> usize {
    process_stereo(resampler, input, output)
}

#[derive(Clone, Copy)]
enum PerfScenario {
    DeinterleaveOverhead,
    DetailedBreakdown,
    PassthroughDetection,
    QualityComparison,
}

#[kithara::test]
#[case("resampler_quality", PerfScenario::QualityComparison)]
#[case("resampler_passthrough", PerfScenario::PassthroughDetection)]
#[case("resampler_deinterleave", PerfScenario::DeinterleaveOverhead)]
#[case("resampler_breakdown", PerfScenario::DetailedBreakdown)]
fn perf_resampler_scenarios(
    perf_interleaved: Vec<f32>,
    perf_planar: [Vec<f32>; 2],
    #[case] label: &'static str,
    #[case] scenario: PerfScenario,
) {
    let _guard = HotpathGuardBuilder::new(label).build();
    match scenario {
        PerfScenario::QualityComparison => {
            let input_spec = AudioSpec::new(2, NonZeroU32::new(48000).expect("test rate"));
            let output_rate = 44100;
            let test_frames = 2048;
            let qualities = [
                ResamplerQuality::Fast,
                ResamplerQuality::Normal,
                ResamplerQuality::Good,
                ResamplerQuality::High,
            ];

            for quality in qualities {
                let mut resampler = build_resampler(
                    input_spec.sample_rate.get(),
                    output_rate,
                    quality,
                    test_frames,
                );
                let input = create_planar(test_frames, &perf_planar);
                let mut output = create_output(&*resampler);

                for _ in 0..10 {
                    let _ = process_stereo(&mut *resampler, &input, &mut output);
                }
                for _ in 0..1000 {
                    resampler_process_single(&mut *resampler, &input, &mut output);
                }

                println!("\n{:=<60}", "");
                println!("Quality: {:?}", quality);
                println!("Iterations: 1000");
                println!("{:=<60}\n", "");
            }
        }
        PerfScenario::PassthroughDetection => {
            let spec = AudioSpec::new(2, NonZeroU32::new(44100).expect("test rate"));
            let mut resampler = build_resampler(
                spec.sample_rate.get(),
                spec.sample_rate.get(),
                ResamplerQuality::Good,
                2048,
            );
            let input = create_planar(2048, &perf_planar);
            let mut output = create_output(&*resampler);

            for _ in 0..10 {
                let _ = process_stereo(&mut *resampler, &input, &mut output);
            }
            for _ in 0..10000 {
                resampler_passthrough_process(&mut *resampler, &input, &mut output);
            }

            println!("\n{:=<60}", "");
            println!("Passthrough Performance Test");
            println!("Iterations: 10000");
            println!("{:=<60}\n", "");
        }
        PerfScenario::DeinterleaveOverhead => {
            let input_spec = AudioSpec::new(2, NonZeroU32::new(48000).expect("test rate"));
            let chunk = create_test_chunk(2048, input_spec, &perf_interleaved);

            for _ in 0..1000 {
                hotpath::measure_block!("deinterleave", {
                    let mut planar: [Vec<f32>; 2] = [Vec::new(), Vec::new()];
                    for (ch, buf) in planar.iter_mut().enumerate() {
                        buf.resize(2048, 0.0);
                        for frame in 0..2048 {
                            buf[frame] = chunk.samples[frame * 2 + ch];
                        }
                    }
                });

                hotpath::measure_block!("interleave", {
                    let mut interleaved = vec![0.0f32; 2048 * 2];
                    let planar: [Vec<f32>; 2] = [Vec::new(), Vec::new()];
                    for frame in 0..2048 {
                        for ch in 0..2 {
                            interleaved[frame * 2 + ch] =
                                planar[ch].get(frame).copied().unwrap_or(0.0);
                        }
                    }
                });
            }

            println!("\n{:=<60}", "");
            println!("Deinterleave/Interleave Overhead");
            println!("Iterations: 1000");
            println!("{:=<60}\n", "");
        }
        PerfScenario::DetailedBreakdown => {
            let input_spec = AudioSpec::new(2, NonZeroU32::new(48000).expect("test rate"));
            let output_rate = 44100;
            let chunk_sizes = [512, 1024, 2048, 4096];

            for &size in &chunk_sizes {
                let mut resampler = build_resampler(
                    input_spec.sample_rate.get(),
                    output_rate,
                    ResamplerQuality::High,
                    size,
                );
                let input = create_planar(size, &perf_planar);
                let mut output = create_output(&*resampler);
                for _ in 0..10 {
                    let _ = process_stereo(&mut *resampler, &input, &mut output);
                }
                for _ in 0..1000 {
                    let _ = resampler_full_process_cycle(&mut *resampler, &input, &mut output);
                }

                println!("\n{:=<60}", "");
                println!("Chunk size: {} frames", size);
                println!("{:=<60}\n", "");
            }
        }
    }
}
