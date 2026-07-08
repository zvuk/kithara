use std::num::{NonZeroU32, NonZeroUsize};

use assert_no_alloc::*;
use kithara::{
    self,
    bufpool::{PcmPool, SharedPool},
    decode::{PcmChunk, PcmMeta, PcmSpec},
    resampler::{
        Resampler, ResamplerConfig, ResamplerMode, ResamplerOptions, ResamplerQuality,
        ResamplerSettings, create_resampler, rubato::RubatoBackend,
    },
};

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
        let _samples: &[f32] = &chunk.samples;
        let _frames = chunk.frames();
        let _spec = chunk.spec();
        if !chunk.samples.is_empty() {
            let _ = chunk.samples[0];
        }
    });

    permit_alloc(|| drop(chunk));
}

fn build_resampler(pool: &PcmPool, source_rate: u32, target_rate: u32) -> Box<dyn Resampler> {
    let settings = ResamplerSettings::builder()
        .channels(NonZeroUsize::new(2).unwrap_or_else(|| panic!("test channels")))
        .mode(ResamplerMode::FixedRatio {
            source_sample_rate: NonZeroU32::new(source_rate)
                .unwrap_or_else(|| panic!("test source rate")),
            target_sample_rate: NonZeroU32::new(target_rate)
                .unwrap_or_else(|| panic!("test target rate")),
        })
        .quality(ResamplerQuality::High)
        .options(ResamplerOptions::builder().chunk_size(4_096).build())
        .pcm_pool(pool.clone())
        .build();
    let config = ResamplerConfig::builder()
        .backend(RubatoBackend::new())
        .settings(settings)
        .build();
    create_resampler(&config).unwrap_or_else(|err| panic!("resampler should build: {err}"))
}

fn planar_block(pool: &PcmPool, frames: usize) -> [kithara::bufpool::PcmBuf; 2] {
    let mut left = pool.get();
    let mut right = pool.get();
    left.ensure_len(frames)
        .unwrap_or_else(|err| panic!("left channel buffer should fit: {err}"));
    right
        .ensure_len(frames)
        .unwrap_or_else(|err| panic!("right channel buffer should fit: {err}"));
    for frame in 0..frames {
        #[expect(
            clippy::cast_precision_loss,
            reason = "test data, precision irrelevant"
        )]
        let phase = frame as f32 * 0.001;
        left[frame] = phase.sin();
        right[frame] = phase.cos();
    }
    [left, right]
}

fn output_block(pool: &PcmPool, frames: usize) -> [kithara::bufpool::PcmBuf; 2] {
    let mut left = pool.get();
    let mut right = pool.get();
    left.ensure_len(frames)
        .unwrap_or_else(|err| panic!("left output buffer should fit: {err}"));
    right
        .ensure_len(frames)
        .unwrap_or_else(|err| panic!("right output buffer should fit: {err}"));
    [left, right]
}

fn process_planar(
    resampler: &mut dyn Resampler,
    input: &[kithara::bufpool::PcmBuf; 2],
    output: &mut [kithara::bufpool::PcmBuf; 2],
) -> usize {
    let input_refs = [&input[0][..], &input[1][..]];
    let (left, right) = output.split_at_mut(1);
    let mut output_refs = [&mut left[0][..], &mut right[0][..]];
    resampler
        .process_into_buffer(&input_refs, &mut output_refs)
        .unwrap_or_else(|err| panic!("resampler process should succeed: {err}"))
        .output_frames
}

/// Active fixed-ratio resampler construction pre-allocates its scratch, so the
/// first process call after construction is allocation-free.
#[kithara::test]
fn resampler_active_first_chunk_alloc_free() {
    let pool = make_pool();

    let (mut resampler, input, mut output) = permit_alloc(|| {
        pool.pre_warm(64, |v| v.resize(16384, 0.0));
        let resampler = build_resampler(&pool, 48_000, 44_100);
        let input = planar_block(&pool, 4_096);
        let output = output_block(&pool, resampler.output_frames_next());
        (resampler, input, output)
    });

    assert_no_alloc(|| {
        let frames = process_planar(&mut *resampler, &input, &mut output);
        assert!(frames > 0);
    });
}

/// Active fixed-ratio resampler stays allocation-free after warmup.
#[kithara::test]
fn resampler_active_steady_state_alloc_free() {
    let pool = make_pool();

    let (mut resampler, input, mut output) = permit_alloc(|| {
        pool.pre_warm(64, |v| v.resize(16384, 0.0));
        let mut resampler = build_resampler(&pool, 48_000, 44_100);
        for _ in 0..16 {
            let warm = planar_block(&pool, 4_096);
            let mut warm_output = output_block(&pool, resampler.output_frames_next());
            let _ = process_planar(&mut *resampler, &warm, &mut warm_output);
        }
        let input = planar_block(&pool, 4_096);
        let output = output_block(&pool, resampler.output_frames_next());
        (resampler, input, output)
    });

    assert_no_alloc(|| {
        let frames = process_planar(&mut *resampler, &input, &mut output);
        assert!(frames > 0);
    });
}

/// Pre-sizing scratch only changes capacity; two independent resamplers fed
/// the same input must emit byte-identical output.
#[kithara::test]
fn resampler_presize_keeps_output_bit_exact() {
    let pool = make_pool();
    pool.pre_warm(64, |v| v.resize(16384, 0.0));

    let render = || -> Vec<f32> {
        let mut resampler = build_resampler(&pool, 48_000, 44_100);
        let mut out = Vec::new();
        for n in 0..12 {
            let mut input = planar_block(&pool, 4_096);
            for (i, s) in input[0].iter_mut().enumerate() {
                #[expect(
                    clippy::cast_precision_loss,
                    reason = "test waveform, precision irrelevant"
                )]
                let v = ((n * 4096 + i) as f32 * 0.0007).sin();
                *s = v;
            }
            let mut output = output_block(&pool, resampler.output_frames_next());
            let frames = process_planar(&mut *resampler, &input, &mut output);
            out.extend_from_slice(&output[0][..frames]);
            out.extend_from_slice(&output[1][..frames]);
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
    let pool = make_pool();

    let (mut resampler, input, mut output) = permit_alloc(|| {
        pool.pre_warm(32, |v| v.resize(8192, 0.0));
        let mut resampler = build_resampler(&pool, 44_100, 44_100);
        let warmup = planar_block(&pool, 4_096);
        let mut warmup_output = output_block(&pool, resampler.output_frames_next());
        let _ = process_planar(&mut *resampler, &warmup, &mut warmup_output);
        let input = planar_block(&pool, 4_096);
        let output = output_block(&pool, resampler.output_frames_next());
        (resampler, input, output)
    });

    assert_no_alloc(|| {
        let frames = process_planar(&mut *resampler, &input, &mut output);
        assert!(frames > 0);
    });
}
