//! Performance tests for audio resampler.
//!
//! Run with: `cargo test --test resampler --features perf --release`

#![cfg(feature = "perf")]

use kithara_audio::{AudioEffect, ResamplerParams, ResamplerQuality};
use kithara_bufpool::pcm_pool;
use kithara_decode::{PcmChunk, PcmSpec};
use std::sync::{atomic::AtomicU32, Arc};

/// Create a test PCM chunk with specified sample count.
fn create_test_chunk(frames: usize, spec: PcmSpec) -> PcmChunk<f32> {
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

    PcmChunk::new(spec, pcm.into_inner())
}

#[hotpath::measure]
fn resampler_process_single(
    resampler: &mut kithara_audio::ResamplerProcessor,
    chunk: &PcmChunk<f32>,
) {
    let _ = resampler.process(chunk.clone());
}

#[test]
#[ignore] // Run explicitly with --ignored
fn perf_resampler_quality_comparison() {
    let _guard = hotpath::GuardBuilder::new("resampler_quality").build();
    let input_spec = PcmSpec {
        sample_rate: 48000,
        channels: 2,
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
        // Create resampler (not included in measurement)
        let host_rate = Arc::new(AtomicU32::new(output_rate));
        let params = ResamplerParams::new(host_rate, input_spec.sample_rate, input_spec.channels as usize)
            .with_quality(quality);
        let mut resampler = kithara_audio::ResamplerProcessor::new(params);

        let chunk = create_test_chunk(test_frames, input_spec);

        // Warm-up (10 iterations)
        for _ in 0..10 {
            let _ = resampler.process(chunk.clone());
        }

        // Measured run (1000 iterations)
        for _ in 0..1000 {
            resampler_process_single(&mut resampler, &chunk);
        }

        println!("\n{:=<60}", "");
        println!("Quality: {:?}", quality);
        println!("Iterations: 1000");
        println!("{:=<60}\n", "");
    }
}

#[hotpath::measure]
fn resampler_passthrough_process(
    resampler: &mut kithara_audio::ResamplerProcessor,
    chunk: &PcmChunk<f32>,
) {
    let _ = resampler.process(chunk.clone());
}

#[test]
#[ignore]
fn perf_resampler_passthrough_detection() {
    let _guard = hotpath::GuardBuilder::new("resampler_passthrough").build();
    let spec = PcmSpec {
        sample_rate: 44100,
        channels: 2,
    };

    // Same rate input/output → should trigger passthrough
    let host_rate = Arc::new(AtomicU32::new(spec.sample_rate));
    let params = ResamplerParams::new(host_rate, spec.sample_rate, spec.channels as usize)
        .with_quality(ResamplerQuality::Good);
    let mut resampler = kithara_audio::ResamplerProcessor::new(params);

    let chunk = create_test_chunk(2048, spec);

    // Warm-up
    for _ in 0..10 {
        let _ = resampler.process(chunk.clone());
    }

    // Measure passthrough performance
    for _ in 0..10000 {
        resampler_passthrough_process(&mut resampler, &chunk);
    }

    println!("\n{:=<60}", "");
    println!("Passthrough Performance Test");
    println!("Iterations: 10000");
    println!("{:=<60}\n", "");
}

#[test]
#[ignore]
fn perf_resampler_deinterleave_overhead() {
    let _guard = hotpath::GuardBuilder::new("resampler_deinterleave").build();
    let input_spec = PcmSpec {
        sample_rate: 48000,
        channels: 2,
    };

    let chunk = create_test_chunk(2048, input_spec);

    // Measure raw deinterleave/interleave overhead
    for _ in 0..1000 {
        hotpath::measure_block!("deinterleave", {
            // Simulate deinterleave operation
            let mut planar: [Vec<f32>; 2] = [Vec::new(), Vec::new()];
            for (ch, buf) in planar.iter_mut().enumerate() {
                buf.resize(2048, 0.0);
                for frame in 0..2048 {
                    buf[frame] = chunk.pcm[frame * 2 + ch];
                }
            }
        });

        hotpath::measure_block!("interleave", {
            // Simulate re-interleave
            let mut interleaved = vec![0.0f32; 2048 * 2];
            let planar: [Vec<f32>; 2] = [Vec::new(), Vec::new()];
            for frame in 0..2048 {
                for ch in 0..2 {
                    interleaved[frame * 2 + ch] = planar[ch].get(frame).copied().unwrap_or(0.0);
                }
            }
        });
    }

    println!("\n{:=<60}", "");
    println!("Deinterleave/Interleave Overhead");
    println!("Iterations: 1000");
    println!("{:=<60}\n", "");
}

/// Detailed breakdown of resampling operations for a single quality preset.
/// Measures: process overhead, actual resampling work, buffer management.
#[hotpath::measure]
fn resampler_full_process_cycle(
    resampler: &mut kithara_audio::ResamplerProcessor,
    chunk: &PcmChunk<f32>,
) -> Option<PcmChunk<f32>> {
    resampler.process(chunk.clone())
}

#[test]
#[ignore]
fn perf_resampler_detailed_breakdown() {
    let _guard = hotpath::GuardBuilder::new("resampler_breakdown").build();

    let input_spec = PcmSpec {
        sample_rate: 48000,
        channels: 2,
    };
    let output_rate = 44100;

    // Test with High quality (significant CPU load)
    let host_rate = Arc::new(AtomicU32::new(output_rate));
    let params = ResamplerParams::new(host_rate, input_spec.sample_rate, input_spec.channels as usize)
        .with_quality(ResamplerQuality::High);
    let mut resampler = kithara_audio::ResamplerProcessor::new(params);

    // Different chunk sizes to see buffer management overhead
    let chunk_sizes = [512, 1024, 2048, 4096];

    for &size in &chunk_sizes {
        let chunk = create_test_chunk(size, input_spec);

        // Warm-up
        for _ in 0..10 {
            let _ = resampler.process(chunk.clone());
        }

        // Measure 1000 iterations per chunk size
        for _ in 0..1000 {
            let _ = resampler_full_process_cycle(&mut resampler, &chunk);
        }

        println!("\n{:=<60}", "");
        println!("Chunk size: {} frames", size);
        println!("{:=<60}\n", "");
    }
}
