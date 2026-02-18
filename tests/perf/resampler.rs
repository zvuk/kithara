//! Performance tests for audio resampler.
//!
//! Run with: `cargo test --test resampler --features perf --release`

#![cfg(feature = "perf")]

use std::sync::{Arc, atomic::AtomicU32};

use kithara::{
    audio::{AudioEffect, ResamplerParams, ResamplerQuality},
    bufpool::pcm_pool,
    decode::{PcmChunk, PcmMeta, PcmSpec},
};
use rstest::rstest;

/// Create a test PCM chunk with specified sample count.
fn create_test_chunk(frames: usize, spec: PcmSpec) -> PcmChunk {
    let samples = frames * spec.channels as usize;
    let pool = pcm_pool();
    let pcm = pool.get_with(|b| {
        b.clear();
        b.resize(samples, 0.0);
        // Fill with sine wave
        for i in 0..samples {
            b[i] = (i as f32 * 0.01).sin() * 0.5;
        }
    });

    PcmChunk::new(
        PcmMeta {
            spec,
            ..Default::default()
        },
        pcm,
    )
}

#[hotpath::measure]
fn resampler_process_single(resampler: &mut kithara::audio::ResamplerProcessor, chunk: &PcmChunk) {
    let _ = resampler.process(chunk.clone());
}

#[hotpath::measure]
fn resampler_passthrough_process(
    resampler: &mut kithara::audio::ResamplerProcessor,
    chunk: &PcmChunk,
) {
    let _ = resampler.process(chunk.clone());
}

/// Detailed breakdown of resampling operations for a single quality preset.
/// Measures: process overhead, actual resampling work, buffer management.
#[hotpath::measure]
fn resampler_full_process_cycle(
    resampler: &mut kithara::audio::ResamplerProcessor,
    chunk: &PcmChunk,
) -> Option<PcmChunk> {
    resampler.process(chunk.clone())
}

#[derive(Clone, Copy)]
enum PerfScenario {
    DeinterleaveOverhead,
    DetailedBreakdown,
    PassthroughDetection,
    QualityComparison,
}

#[rstest]
#[case("resampler_quality", PerfScenario::QualityComparison)]
#[case("resampler_passthrough", PerfScenario::PassthroughDetection)]
#[case("resampler_deinterleave", PerfScenario::DeinterleaveOverhead)]
#[case("resampler_breakdown", PerfScenario::DetailedBreakdown)]
#[test]
#[ignore]
fn perf_resampler_scenarios(#[case] label: &'static str, #[case] scenario: PerfScenario) {
    let _guard = hotpath::FunctionsGuardBuilder::new(label).build();
    match scenario {
        PerfScenario::QualityComparison => {
            let input_spec = PcmSpec {
                channels: 2,
                sample_rate: 48000,
            };
            let output_rate = 44100;
            let test_frames = 2048;
            let qualities = [
                ResamplerQuality::Fast,
                ResamplerQuality::Normal,
                ResamplerQuality::Good,
                ResamplerQuality::High,
                ResamplerQuality::Maximum,
            ];

            for quality in qualities {
                let host_rate = Arc::new(AtomicU32::new(output_rate));
                let params = ResamplerParams::new(
                    host_rate,
                    input_spec.sample_rate,
                    input_spec.channels as usize,
                )
                .with_quality(quality);
                let mut resampler = kithara::audio::ResamplerProcessor::new(params);
                let chunk = create_test_chunk(test_frames, input_spec);

                for _ in 0..10 {
                    let _ = resampler.process(chunk.clone());
                }
                for _ in 0..1000 {
                    resampler_process_single(&mut resampler, &chunk);
                }

                println!("\n{:=<60}", "");
                println!("Quality: {:?}", quality);
                println!("Iterations: 1000");
                println!("{:=<60}\n", "");
            }
        }
        PerfScenario::PassthroughDetection => {
            let spec = PcmSpec {
                channels: 2,
                sample_rate: 44100,
            };
            let host_rate = Arc::new(AtomicU32::new(spec.sample_rate));
            let params = ResamplerParams::new(host_rate, spec.sample_rate, spec.channels as usize)
                .with_quality(ResamplerQuality::Good);
            let mut resampler = kithara::audio::ResamplerProcessor::new(params);
            let chunk = create_test_chunk(2048, spec);

            for _ in 0..10 {
                let _ = resampler.process(chunk.clone());
            }
            for _ in 0..10000 {
                resampler_passthrough_process(&mut resampler, &chunk);
            }

            println!("\n{:=<60}", "");
            println!("Passthrough Performance Test");
            println!("Iterations: 10000");
            println!("{:=<60}\n", "");
        }
        PerfScenario::DeinterleaveOverhead => {
            let input_spec = PcmSpec {
                channels: 2,
                sample_rate: 48000,
            };
            let chunk = create_test_chunk(2048, input_spec);

            for _ in 0..1000 {
                hotpath::measure_block!("deinterleave", {
                    let mut planar: [Vec<f32>; 2] = [Vec::new(), Vec::new()];
                    for (ch, buf) in planar.iter_mut().enumerate() {
                        buf.resize(2048, 0.0);
                        for frame in 0..2048 {
                            buf[frame] = chunk.pcm[frame * 2 + ch];
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
            let input_spec = PcmSpec {
                channels: 2,
                sample_rate: 48000,
            };
            let output_rate = 44100;
            let host_rate = Arc::new(AtomicU32::new(output_rate));
            let params = ResamplerParams::new(
                host_rate,
                input_spec.sample_rate,
                input_spec.channels as usize,
            )
            .with_quality(ResamplerQuality::High);
            let mut resampler = kithara::audio::ResamplerProcessor::new(params);
            let chunk_sizes = [512, 1024, 2048, 4096];

            for &size in &chunk_sizes {
                let chunk = create_test_chunk(size, input_spec);
                for _ in 0..10 {
                    let _ = resampler.process(chunk.clone());
                }
                for _ in 0..1000 {
                    let _ = resampler_full_process_cycle(&mut resampler, &chunk);
                }

                println!("\n{:=<60}", "");
                println!("Chunk size: {} frames", size);
                println!("{:=<60}\n", "");
            }
        }
    }
}
