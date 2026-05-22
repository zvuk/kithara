use std::sync::{Arc, atomic::AtomicU32};

use assert_no_alloc::*;
use kithara_bufpool::{PcmPool, SharedPool};
use kithara_decode::{PcmChunk, PcmMeta, PcmSpec};
use kithara_test_utils::kithara;

#[cfg(debug_assertions)]
#[global_allocator]
static A: AllocDisabler = AllocDisabler;

fn make_pool() -> PcmPool {
    SharedPool::<8, Vec<f32>>::new(128, 200_000)
}

fn make_chunk(pool: &PcmPool, frames: usize, channels: u16) -> PcmChunk {
    let samples = frames * channels as usize;
    let mut pcm = pool.get_with(|v| v.resize(samples, 0.0));
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
        ..Default::default()
    };
    PcmChunk::new(meta, pcm)
}

#[kithara::test]
fn test_pool_get_put_allocation_free() {
    let pool = make_pool();

    permit_alloc(|| {
        pool.pre_warm(16, |v| v.resize(4096, 0.0));
        for _ in 0..20 {
            let _buf = pool.get();
        }
    });

    assert_no_alloc(|| {
        for _ in 0..10 {
            let _buf = pool.get();
        }
    });
}

#[kithara::test]
fn test_pcm_chunk_access_allocation_free() {
    let pool = make_pool();

    let chunk = permit_alloc(|| {
        pool.pre_warm(16, |v| v.resize(4096, 0.0));
        make_chunk(&pool, 1024, 2)
    });

    assert_no_alloc(|| {
        let _samples: &[f32] = &chunk.pcm;
        let _frames = chunk.frames();
        let _spec = chunk.spec();
        if !chunk.pcm.is_empty() {
            let _ = chunk.pcm[0];
        }
    });

    permit_alloc(|| drop(chunk));
}

#[kithara::test]
fn test_resampler_passthrough_allocation_free() {
    use kithara_audio::{AudioEffect, ResamplerParams, ResamplerProcessor};

    let pool = make_pool();

    let (mut processor, chunk) = permit_alloc(|| {
        pool.pre_warm(32, |v| v.resize(8192, 0.0));

        let host_rate = Arc::new(AtomicU32::new(44100));
        let params = ResamplerParams::builder()
            .host_sample_rate(host_rate)
            .source_sample_rate(44100)
            .channels(2)
            .pool(pool.clone())
            .build();
        let processor = ResamplerProcessor::new(params);

        let chunk = make_chunk(&pool, 4096, 2);

        let warmup_chunk = make_chunk(&pool, 4096, 2);
        let mut warmup_proc = {
            let host_rate = Arc::new(AtomicU32::new(44100));
            let params = ResamplerParams::builder()
                .host_sample_rate(host_rate)
                .source_sample_rate(44100)
                .channels(2)
                .pool(pool.clone())
                .build();
            ResamplerProcessor::new(params)
        };
        let _ = warmup_proc.process(warmup_chunk);

        (processor, chunk)
    });

    assert_no_alloc(|| {
        let result = processor.process(chunk);
        if let Some(output) = result {
            let _ = output.pcm.len();
            drop(output);
        }
    });
}
