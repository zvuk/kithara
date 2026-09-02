use std::num::{NonZeroU32, NonZeroUsize};

use assert_no_alloc::*;
use kithara::{
    self,
    bufpool::PoolConfig,
    resampler::{
        Resampler, ResamplerConfig, ResamplerMode, ResamplerOptions, ResamplerQuality,
        ResamplerSettings, create_resampler, rubato::RubatoBackend,
    },
    signal::{AudioChunk, AudioChunkInfo, AudioSpec},
    warp::{StretchControls, StretchKind, Warp, WarpConfig, WarpRenderer},
};
use kithara_integration_tests::bufpool_ext::{Pools, TestPools, pools_with};

#[cfg(debug_assertions)]
#[global_allocator]
static A: AllocDisabler = AllocDisabler;

fn make_pools() -> Pools {
    eager_pools(0, 0)
}

fn eager_pools(initial_buffers: usize, initial_capacity: usize) -> Pools {
    pools_with(
        64 * 1024 * 1024,
        PoolConfig::builder().max_buffers(32).build(),
        PoolConfig::builder()
            .initial_buffers(initial_buffers)
            .initial_capacity(initial_capacity)
            .max_buffers(128)
            .build(),
    )
}

fn warp_renderer(
    controls: kithara::platform::sync::Arc<StretchControls>,
    spec: AudioSpec,
    pools: Pools,
) -> WarpRenderer<TestPools> {
    let config = WarpConfig::builder().stretch(controls).build();
    Warp::new((), &config).renderer(spec, pools)
}

fn make_chunk(pools: &Pools, frames: usize, channels: u16) -> AudioChunk {
    make_chunk_at(pools, frames, channels, 44100, 0)
}

fn make_chunk_at(
    pools: &Pools,
    frames: usize,
    channels: u16,
    sample_rate: u32,
    frame_offset: u64,
) -> AudioChunk {
    let samples = frames * channels as usize;
    let mut pcm = pools
        .get_with_len::<f32>(samples)
        .unwrap_or_else(|error| panic!("test sample buffer: {error}"));
    for (i, s) in pcm.iter_mut().enumerate() {
        #[expect(
            clippy::cast_precision_loss,
            reason = "test data, precision irrelevant"
        )]
        let val = (i as f32) * 0.001;
        *s = val;
    }
    let spec = AudioSpec::new(channels, NonZeroU32::new(sample_rate).expect("test rate"));
    let frame_count = u64::try_from(frames).unwrap_or_else(|error| panic!("test frames: {error}"));
    let meta = AudioChunkInfo {
        frame_offset,
        timestamp: spec
            .duration_for(frame_offset)
            .unwrap_or_else(|error| panic!("test timestamp: {error}")),
        end_timestamp: spec
            .duration_for(frame_offset + frame_count)
            .unwrap_or_else(|error| panic!("test duration: {error}")),
        frames: u32::try_from(frames).unwrap_or_else(|error| panic!("test frames: {error}")),
        spec,
        ..Default::default()
    };
    AudioChunk::new(meta, pcm)
}

#[kithara::test]
fn test_pool_get_put_allocation_free() {
    let pools = eager_pools(16, 4_096);

    permit_alloc(|| {
        for _ in 0..20 {
            let _buf = pools.get::<f32>();
        }
    });

    assert_no_alloc(|| {
        for _ in 0..10 {
            let _buf = pools.get::<f32>();
        }
    });
}

#[kithara::test]
fn test_pcm_chunk_access_allocation_free() {
    let pools = eager_pools(16, 4_096);

    let chunk = permit_alloc(|| make_chunk(&pools, 1024, 2));

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

fn build_resampler(pools: &Pools, source_rate: u32, target_rate: u32) -> impl Resampler {
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
        .pools(pools.clone())
        .build();
    let config = ResamplerConfig::builder()
        .backend(RubatoBackend::new())
        .settings(settings)
        .build();
    create_resampler(&config).unwrap_or_else(|err| panic!("resampler should build: {err}"))
}

fn planar_block(pools: &Pools, frames: usize) -> [kithara::bufpool::SampleBuffer; 2] {
    let mut left = pools.get::<f32>();
    let mut right = pools.get::<f32>();
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

fn output_block(pools: &Pools, frames: usize) -> [kithara::bufpool::SampleBuffer; 2] {
    let mut left = pools.get::<f32>();
    let mut right = pools.get::<f32>();
    left.ensure_len(frames)
        .unwrap_or_else(|err| panic!("left output buffer should fit: {err}"));
    right
        .ensure_len(frames)
        .unwrap_or_else(|err| panic!("right output buffer should fit: {err}"));
    [left, right]
}

fn process_planar(
    resampler: &mut dyn Resampler,
    input: &[kithara::bufpool::SampleBuffer; 2],
    output: &mut [kithara::bufpool::SampleBuffer; 2],
) -> usize {
    let input_refs = [&input[0][..], &input[1][..]];
    let (left, right) = output.split_at_mut(1);
    let mut output_refs = [&mut left[0][..], &mut right[0][..]];
    resampler
        .process_into_buffer(&input_refs, &mut output_refs)
        .unwrap_or_else(|err| panic!("resampler process should succeed: {err}"))
        .output_frames
}

#[kithara::test]
fn resampler_active_first_chunk_alloc_free() {
    let pools = eager_pools(64, 16_384);

    let (mut resampler, input, mut output) = permit_alloc(|| {
        let resampler = build_resampler(&pools, 48_000, 44_100);
        let input = planar_block(&pools, 4_096);
        let output = output_block(&pools, resampler.output_frames_next());
        (resampler, input, output)
    });

    assert_no_alloc(|| {
        let frames = process_planar(&mut resampler, &input, &mut output);
        assert!(frames > 0);
    });
}

#[kithara::test]
fn resampler_active_steady_state_alloc_free() {
    let pools = eager_pools(64, 16_384);

    let (mut resampler, input, mut output) = permit_alloc(|| {
        let mut resampler = build_resampler(&pools, 48_000, 44_100);
        for _ in 0..16 {
            let warm = planar_block(&pools, 4_096);
            let mut warm_output = output_block(&pools, resampler.output_frames_next());
            let _ = process_planar(&mut resampler, &warm, &mut warm_output);
        }
        let input = planar_block(&pools, 4_096);
        let output = output_block(&pools, resampler.output_frames_next());
        (resampler, input, output)
    });

    assert_no_alloc(|| {
        let frames = process_planar(&mut resampler, &input, &mut output);
        assert!(frames > 0);
    });
}

#[kithara::test]
fn resampler_presize_keeps_output_bit_exact() {
    let pools = eager_pools(64, 16_384);

    let render = || -> Vec<f32> {
        let mut resampler = build_resampler(&pools, 48_000, 44_100);
        let mut out = Vec::new();
        for n in 0..12 {
            let mut input = planar_block(&pools, 4_096);
            for (i, s) in input[0].iter_mut().enumerate() {
                #[expect(
                    clippy::cast_precision_loss,
                    reason = "test waveform, precision irrelevant"
                )]
                let v = ((n * 4096 + i) as f32 * 0.0007).sin();
                *s = v;
            }
            let mut output = output_block(&pools, resampler.output_frames_next());
            let frames = process_planar(&mut resampler, &input, &mut output);
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
    let pools = eager_pools(32, 8_192);

    let (mut resampler, input, mut output) = permit_alloc(|| {
        let mut resampler = build_resampler(&pools, 44_100, 44_100);
        let warmup = planar_block(&pools, 4_096);
        let mut warmup_output = output_block(&pools, resampler.output_frames_next());
        let _ = process_planar(&mut resampler, &warmup, &mut warmup_output);
        let input = planar_block(&pools, 4_096);
        let output = output_block(&pools, resampler.output_frames_next());
        (resampler, input, output)
    });

    assert_no_alloc(|| {
        let frames = process_planar(&mut resampler, &input, &mut output);
        assert!(frames > 0);
    });
}

#[kithara::test]
#[case(StretchKind::Signalsmith)]
#[cfg_attr(
    not(all(target_os = "windows", target_env = "msvc")),
    case(StretchKind::Bungee)
)]
fn timestretch_active_process_and_terminal_flush_are_allocation_free(#[case] kind: StretchKind) {
    const FRAMES: usize = 8_192;
    let pools = make_pools();
    let spec = AudioSpec::new(2, NonZeroU32::new(44_100).expect("test rate"));
    let (mut effect, first, second) = permit_alloc(|| {
        let controls = StretchControls::new(0.5);
        controls.set_keylock(true);
        controls.set_backend(kind);
        let mut effect = warp_renderer(controls, spec, pools.clone());
        effect.prepare(spec);
        let first = make_chunk_at(&pools, FRAMES, 2, spec.sample_rate.get(), 0);
        let second = make_chunk_at(
            &pools,
            FRAMES,
            2,
            spec.sample_rate.get(),
            u64::try_from(FRAMES).unwrap_or_else(|error| panic!("test frame offset: {error}")),
        );
        (effect, first, second)
    });

    let first_output = assert_no_alloc(|| {
        effect
            .render(first)
            .unwrap_or_else(|| panic!("active stretch must render"))
    });
    permit_alloc(|| {
        effect.prepare(spec);
        drop(first_output);
    });

    let second_output = assert_no_alloc(|| {
        effect
            .render(second)
            .unwrap_or_else(|| panic!("serviced stretch must render again"))
    });
    permit_alloc(|| {
        effect.prepare(spec);
        drop(second_output);
    });

    let terminal = assert_no_alloc(|| effect.flush());
    permit_alloc(|| {
        effect.prepare(spec);
        drop(terminal);
    });
}

#[kithara::test]
#[case(StretchKind::Signalsmith)]
#[cfg_attr(
    not(all(target_os = "windows", target_env = "msvc")),
    case(StretchKind::Bungee)
)]
fn timestretch_pending_and_maximum_output_are_allocation_free(#[case] kind: StretchKind) {
    const FRAMES: usize = 8_192;
    let pools = make_pools();
    let spec = AudioSpec::new(2, NonZeroU32::new(44_100).expect("test rate"));
    let (mut maximum, input) = permit_alloc(|| {
        let controls = StretchControls::new(0.05);
        controls.set_keylock(true);
        controls.set_backend(kind);
        let mut maximum = warp_renderer(controls, spec, pools.clone());
        maximum.prepare(spec);
        let input = make_chunk(&pools, FRAMES, 2);
        (maximum, input)
    });
    let maximum_output = assert_no_alloc(|| {
        maximum
            .render(input)
            .unwrap_or_else(|| panic!("maximum prepared output must render"))
    });
    assert_eq!(maximum_output.frames(), 163_840);
    permit_alloc(|| {
        maximum.prepare(spec);
        drop(maximum_output);
    });

    let (mut pending, input) = permit_alloc(|| {
        let controls = StretchControls::new(2.0);
        controls.set_keylock(true);
        controls.set_backend(kind);
        let mut pending = warp_renderer(controls, spec, pools.clone());
        pending.prepare(spec);
        let input = make_chunk(&pools, 1, 2);
        (pending, input)
    });
    assert_no_alloc(|| {
        assert!(pending.render(input).is_none());
    });
    permit_alloc(|| pending.prepare(spec));

    let terminal = assert_no_alloc(|| {
        pending
            .flush()
            .unwrap_or_else(|| panic!("pending frame plus terminal tail must render"))
    });
    permit_alloc(|| {
        pending.prepare(spec);
        drop(terminal);
    });
}
