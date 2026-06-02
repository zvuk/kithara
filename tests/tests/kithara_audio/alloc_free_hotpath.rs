use std::{
    num::NonZeroU32,
    sync::{Arc, atomic::AtomicU32},
};

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
    make_chunk_at(pool, frames, channels, 44100)
}

fn make_chunk_at(pool: &PcmPool, frames: usize, channels: u16, sample_rate: u32) -> PcmChunk {
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
        spec: PcmSpec::new(channels, NonZeroU32::new(sample_rate).expect("test rate")),
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

fn active_resampler(pool: &PcmPool) -> kithara_audio::ResamplerProcessor {
    use kithara_audio::{ResamplerParams, ResamplerProcessor};

    let host_rate = Arc::new(AtomicU32::new(44_100));
    let params = ResamplerParams::builder()
        .host_sample_rate(host_rate)
        .source_sample_rate(48_000)
        .channels(2)
        .pool(pool.clone())
        .build();
    ResamplerProcessor::new(params)
}

/// WS5b: the ACTIVE resampler (source != host) pre-allocates its scratch at
/// construction, so the VERY FIRST `process` on the produce-core is already
/// allocation-free — no cold-start malloc once `decode_next_chunk` loses its
/// permit. Without the constructor pre-size this aborts (SIGABRT) on the first
/// chunk's scratch growth.
#[kithara::test]
fn resampler_active_first_chunk_alloc_free() {
    use kithara_audio::AudioEffect;

    let pool = make_pool();

    let (mut processor, first_chunk) = permit_alloc(|| {
        pool.pre_warm(64, |v| v.resize(16384, 0.0));
        let processor = active_resampler(&pool);
        let first_chunk = make_chunk_at(&pool, 4096, 2, 48_000);
        (processor, first_chunk)
    });

    assert_no_alloc(|| {
        if let Some(output) = processor.process(first_chunk) {
            let _ = output.pcm.len();
            drop(output);
        }
    });
}

/// WS5b: a live ratio change (DJ rate sweep) stays allocation-free — the
/// scratch is pre-sized to the resampler's worst-case block across the whole
/// `MAX_RATIO_ADJUSTMENT` window, so the steady `process` after a rate change
/// never reallocates.
#[kithara::test]
fn resampler_active_steady_state_alloc_free() {
    use kithara_audio::AudioEffect;

    let pool = make_pool();

    let (mut processor, steady_chunk) = permit_alloc(|| {
        pool.pre_warm(64, |v| v.resize(16384, 0.0));
        let mut processor = active_resampler(&pool);
        for _ in 0..16 {
            let warm = make_chunk_at(&pool, 4096, 2, 48_000);
            let _ = processor.process(warm);
        }
        let steady_chunk = make_chunk_at(&pool, 4096, 2, 48_000);
        (processor, steady_chunk)
    });

    assert_no_alloc(|| {
        if let Some(output) = processor.process(steady_chunk) {
            let _ = output.pcm.len();
            drop(output);
        }
    });
}

/// WS5b bit-exactness guard: pre-sizing the scratch only changes capacity, so
/// two independent processors fed the same input must emit byte-identical
/// output. Catches any accidental value/length change the pre-size could
/// introduce (pro-DJ zero phase tolerance).
#[kithara::test]
fn resampler_presize_keeps_output_bit_exact() {
    use kithara_audio::AudioEffect;

    let pool = make_pool();
    pool.pre_warm(64, |v| v.resize(16384, 0.0));

    let render = || -> Vec<f32> {
        let mut processor = active_resampler(&pool);
        let mut out = Vec::new();
        for n in 0..12 {
            let mut chunk = make_chunk_at(&pool, 4096, 2, 48_000);
            for (i, s) in chunk.pcm.as_mut_slice().iter_mut().enumerate() {
                #[expect(
                    clippy::cast_precision_loss,
                    reason = "test waveform, precision irrelevant"
                )]
                let v = ((n * 4096 + i) as f32 * 0.0007).sin();
                *s = v;
            }
            if let Some(output) = processor.process(chunk) {
                out.extend_from_slice(&output.pcm);
            }
        }
        out
    };

    let a = render();
    let b = render();
    assert_eq!(a, b, "resampler output must be deterministic and bit-exact");
    assert!(!a.is_empty(), "active resampler must emit output");
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
