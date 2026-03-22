use std::{
    sync::{Arc, atomic::AtomicU32},
    time::Duration,
};

use assert_no_alloc::*;
use kithara_bufpool::{PcmPool, SharedPool};
use kithara_decode::{PcmChunk, PcmMeta, PcmSpec};
use kithara_test_utils::kithara;

#[cfg(debug_assertions)]
#[global_allocator]
static A: AllocDisabler = AllocDisabler;

// Tier 3: Zero-allocation audio hot path tests
// These tests verify that pool get/put and PCM chunk access cause no heap
// allocations after warmup. Requires debug_assertions (cargo test default).
// Run with --test-threads=1 to avoid interference.

fn make_pool() -> PcmPool {
    SharedPool::<8, Vec<f32>>::new(128, 200_000)
}

fn make_chunk(pool: &PcmPool, frames: usize, channels: u16) -> PcmChunk {
    let samples = frames * channels as usize;
    let mut pcm = pool.get_with(|v| v.resize(samples, 0.0));
    // Fill with non-zero data to simulate real audio
    for (i, s) in pcm.iter_mut().enumerate() {
        #[expect(
            clippy::cast_precision_loss,
            reason = "test data, precision irrelevant"
        )]
        let val = (i as f32) * 0.001;
        *s = val;
    }
    let meta = PcmMeta {
        spec: PcmSpec {
            channels,
            sample_rate: 44100,
        },
        timestamp: Duration::ZERO,
        segment_index: None,
        variant_index: None,
        epoch: 0,
        frame_offset: 0,
    };
    PcmChunk::new(meta, pcm)
}

#[kithara::test]
fn test_pool_get_put_allocation_free() {
    let pool = make_pool();

    // Warmup: allocate buffers and return them to pool
    permit_alloc(|| {
        pool.pre_warm(16, |v| v.resize(4096, 0.0));
        // Extra warmup cycles to stabilize
        for _ in 0..20 {
            let _buf = pool.get();
        }
    });

    // Steady-state get/drop should be allocation-free
    assert_no_alloc(|| {
        for _ in 0..10 {
            let _buf = pool.get();
            // buf dropped back to pool
        }
    });
}

#[kithara::test]
fn test_pcm_chunk_access_allocation_free() {
    let pool = make_pool();

    // Warmup and create chunk
    let chunk = permit_alloc(|| {
        pool.pre_warm(16, |v| v.resize(4096, 0.0));
        make_chunk(&pool, 1024, 2)
    });

    // Accessing samples, frames, spec should never allocate
    assert_no_alloc(|| {
        let _samples = chunk.samples();
        let _frames = chunk.frames();
        let _spec = chunk.spec();
        // Read some values to prevent optimization
        if !chunk.samples().is_empty() {
            let _ = chunk.samples()[0];
        }
    });

    // Explicitly drop outside assert_no_alloc (drop may deallocate if pool is full)
    permit_alloc(|| drop(chunk));
}

#[kithara::test]
fn test_resampler_passthrough_allocation_free() {
    use kithara_audio::{AudioEffect, ResamplerParams, ResamplerProcessor};

    let pool = make_pool();

    // Create resampler in passthrough mode (same sample rate)
    let (mut processor, chunk) = permit_alloc(|| {
        pool.pre_warm(32, |v| v.resize(8192, 0.0));

        let host_rate = Arc::new(AtomicU32::new(44100));
        let params = ResamplerParams::new(host_rate, 44100, 2).with_pool(Some(pool.clone()));
        let processor = ResamplerProcessor::new(params);

        // Create input chunk
        let chunk = make_chunk(&pool, 4096, 2);

        // Warmup: process one chunk to stabilize internal state
        let warmup_chunk = make_chunk(&pool, 4096, 2);
        let mut warmup_proc = {
            let host_rate = Arc::new(AtomicU32::new(44100));
            let params = ResamplerParams::new(host_rate, 44100, 2).with_pool(Some(pool.clone()));
            ResamplerProcessor::new(params)
        };
        let _ = warmup_proc.process(warmup_chunk);

        (processor, chunk)
    });

    // Passthrough mode: same sample rate → should be zero allocations
    assert_no_alloc(|| {
        let result = processor.process(chunk);
        // In passthrough mode, the chunk passes through without resampling
        if let Some(output) = result {
            let _ = output.samples().len();
            // Drop output inside assert_no_alloc — it returns to pool (no alloc)
            drop(output);
        }
    });
}
