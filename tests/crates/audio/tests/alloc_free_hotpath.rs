use std::num::{NonZeroU32, NonZeroUsize};

use assert_no_alloc::*;
use kithara::{
    self,
    bufpool::{PoolConfig, SampleBuffer},
    resampler::{
        Resampler, ResamplerConfig, ResamplerControl, ResamplerMode, ResamplerOptions,
        ResamplerQuality, ResamplerSettings, create_resampler, glide::GlideBackend,
        rubato::RubatoBackend,
    },
    signal::{AudioChunk, AudioChunkInfo, AudioSpec, FrameCount, InterleavedView, PlanarBuffer},
    warp::{StretchKind, Warp, WarpConfig, WarpRenderer},
};
use kithara_integration_tests::bufpool_ext::{Pools, TestPools, pools_with};
use kithara_test_fixtures::integration_fixtures::{
    allocation_planar, allocation_ramp, allocation_sequence,
};

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

/// A renderer starting at `speed` on keylocked `kind`.
fn warp_renderer(
    speed: f32,
    kind: StretchKind,
    spec: AudioSpec,
    pools: Pools,
) -> WarpRenderer<TestPools> {
    let config = WarpConfig::builder()
        .speed(speed)
        .backend(kind)
        .keylock(true)
        .build();
    Warp::new((), &config).renderer(spec, pools)
}

fn make_chunk(pools: &Pools, frames: usize, channels: u16, input: &[f32]) -> AudioChunk {
    make_chunk_at(pools, frames, channels, 44100, input)
}

fn make_chunk_at(
    pools: &Pools,
    frames: usize,
    channels: u16,
    sample_rate: u32,
    input: &[f32],
) -> AudioChunk {
    let samples = frames * channels as usize;
    let mut pcm = pools
        .get_with_len::<f32>(samples)
        .unwrap_or_else(|error| panic!("test sample buffer: {error}"));
    pcm.copy_from_slice(&input[..samples]);
    let meta = AudioChunkInfo {
        spec: AudioSpec::new(channels, NonZeroU32::new(sample_rate).expect("test rate")),
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
fn test_pcm_chunk_access_allocation_free(allocation_ramp: Vec<f32>) {
    let pools = eager_pools(16, 4_096);

    let chunk = permit_alloc(|| make_chunk(&pools, 1024, 2, &allocation_ramp));

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

#[kithara::test]
fn planar_append_and_interleave_are_allocation_free_past_eight_channels(allocation_ramp: Vec<f32>) {
    const CHANNELS: u16 = 12;
    const FRAMES: usize = 256;
    let pools = eager_pools(4, 16_384);
    let spec = AudioSpec::new(CHANNELS, NonZeroU32::new(44_100).expect("test rate"));
    let samples = FRAMES * usize::from(CHANNELS);
    let (mut planar, mut output) = permit_alloc(|| {
        let mut planar = PlanarBuffer::new(&pools, spec, FrameCount::new(2 * FRAMES))
            .unwrap_or_else(|error| panic!("reserved planar storage: {error}"));
        planar.clear();
        (planar, vec![0.0_f32; samples])
    });
    let appended = InterleavedView::new(&allocation_ramp[..samples], spec, FrameCount::new(FRAMES))
        .unwrap_or_else(|error| panic!("fixture shape: {error}"));

    assert_no_alloc(|| {
        planar
            .append_interleaved(appended)
            .unwrap_or_else(|error| panic!("reserved storage holds the frames: {error}"));
        planar
            .view()
            .interleave_into(&mut output)
            .unwrap_or_else(|error| panic!("output holds the frames: {error}"));
    });
    assert_eq!(output, allocation_ramp[..samples]);
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

fn build_glide(pools: &Pools) -> impl ResamplerControl {
    let settings = ResamplerSettings::builder()
        .channels(NonZeroUsize::new(2).unwrap_or_else(|| panic!("test channels")))
        .mode(ResamplerMode::VariableRatio {
            sample_rate: NonZeroU32::new(44_100).unwrap_or_else(|| panic!("test rate")),
            initial_ratio: 1.0,
            glide: None,
        })
        .options(ResamplerOptions::builder().chunk_size(4_096).build())
        .pools(pools.clone())
        .build();
    let config = ResamplerConfig::builder()
        .backend(GlideBackend::new())
        .settings(settings)
        .build();
    create_resampler(&config).unwrap_or_else(|err| panic!("glide resampler should build: {err}"))
}

fn stereo_block(pools: &Pools, frames: usize) -> [SampleBuffer; 2] {
    std::array::from_fn(|channel| {
        let mut buffer = pools.get::<f32>();
        buffer
            .ensure_len(frames)
            .unwrap_or_else(|err| panic!("channel {channel} buffer should fit: {err}"));
        buffer
    })
}

fn planar_block(pools: &Pools, frames: usize, input: &[f32]) -> [SampleBuffer; 2] {
    let [mut left, mut right] = stereo_block(pools, frames);
    left.copy_from_slice(&input[..frames]);
    right.copy_from_slice(&input[4_096..4_096 + frames]);
    [left, right]
}

fn process_planar(
    resampler: &mut dyn Resampler,
    input: &[SampleBuffer; 2],
    output: &mut [SampleBuffer; 2],
) -> usize {
    let input_refs = [&input[0][..], &input[1][..]];
    let (left, right) = output.split_at_mut(1);
    let mut output_refs = [&mut left[0][..], &mut right[0][..]];
    resampler
        .process_into_buffer(&input_refs, &mut output_refs)
        .unwrap_or_else(|err| panic!("resampler process should succeed: {err}"))
        .second()
}

#[kithara::test]
#[case::active_first_chunk(48_000, 0, 64, 16_384)]
#[case::active_steady_state(48_000, 16, 64, 16_384)]
#[case::passthrough(44_100, 1, 32, 8_192)]
fn resampler_process_is_allocation_free(
    allocation_planar: Vec<f32>,
    #[case] source_rate: u32,
    #[case] warmup_chunks: usize,
    #[case] initial_buffers: usize,
    #[case] initial_capacity: usize,
) {
    let pools = eager_pools(initial_buffers, initial_capacity);

    let (mut resampler, input, mut output) = permit_alloc(|| {
        let mut resampler = build_resampler(&pools, source_rate, 44_100);
        for _ in 0..warmup_chunks {
            let warm = planar_block(&pools, 4_096, &allocation_planar);
            let mut warm_output = stereo_block(&pools, resampler.output_frames_next());
            let _ = process_planar(&mut resampler, &warm, &mut warm_output);
        }
        let input = planar_block(&pools, 4_096, &allocation_planar);
        let output = stereo_block(&pools, resampler.output_frames_next());
        (resampler, input, output)
    });

    assert_no_alloc(|| {
        let frames = process_planar(&mut resampler, &input, &mut output);
        assert!(frames > 0);
    });
}

/// Leaving passthrough settles the filter; `1.25` and `0.8` retune it on
/// both sides of unity.
#[kithara::test]
fn glide_resampler_process_is_allocation_free(allocation_planar: Vec<f32>) {
    let pools = eager_pools(64, 16_384);

    let (mut resampler, input, mut output) = permit_alloc(|| {
        let mut resampler = build_glide(&pools);
        let warm = planar_block(&pools, 4_096, &allocation_planar);
        let mut warm_output = stereo_block(&pools, 8_192);
        let _ = process_planar(&mut resampler, &warm, &mut warm_output);
        let input = planar_block(&pools, 4_096, &allocation_planar);
        let output = stereo_block(&pools, 8_192);
        (resampler, input, output)
    });

    assert_no_alloc(|| {
        for ratio in [1.25, 0.8] {
            ResamplerControl::set_ratio(&mut resampler, ratio)
                .unwrap_or_else(|err| panic!("ratio {ratio} should be accepted: {err}"));
            let frames = process_planar(&mut resampler, &input, &mut output);
            assert!(frames > 0, "ratio {ratio} rendered nothing");
        }
    });
}

#[kithara::test]
fn resampler_presize_keeps_output_bit_exact(
    allocation_planar: Vec<f32>,
    allocation_sequence: Vec<f32>,
) {
    let pools = eager_pools(64, 16_384);

    let render = || -> Vec<f32> {
        let mut resampler = build_resampler(&pools, 48_000, 44_100);
        let mut out = Vec::new();
        for n in 0..12 {
            let mut input = planar_block(&pools, 4_096, &allocation_planar);
            input[0].copy_from_slice(&allocation_sequence[n * 4096..(n + 1) * 4096]);
            let mut output = stereo_block(&pools, resampler.output_frames_next());
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
#[case(StretchKind::Signalsmith)]
#[cfg_attr(
    all(
        not(target_os = "android"),
        not(all(target_os = "windows", target_env = "msvc"))
    ),
    case(StretchKind::Bungee)
)]
fn timestretch_active_process_and_terminal_flush_are_allocation_free(
    allocation_ramp: Vec<f32>,
    #[case] kind: StretchKind,
) {
    const FRAMES: usize = 8_192;
    let pools = make_pools();
    let spec = AudioSpec::new(2, NonZeroU32::new(44_100).expect("test rate"));
    let (mut effect, first, second) = permit_alloc(|| {
        let mut effect = warp_renderer(0.5, kind, spec, pools.clone());
        effect.prepare(spec);
        let first = make_chunk(&pools, FRAMES, 2, &allocation_ramp);
        let second = make_chunk(&pools, FRAMES, 2, &allocation_ramp);
        (effect, first, second)
    });

    let first_output = assert_no_alloc(|| {
        effect
            .render(first)
            .continue_value()
            .expect("prepared source must be admitted")
            .unwrap_or_else(|| panic!("active stretch must render"))
    });
    permit_alloc(|| {
        effect.prepare(spec);
        drop(first_output);
    });

    let second_output = assert_no_alloc(|| {
        effect
            .render(second)
            .continue_value()
            .expect("prepared source must be admitted")
            .unwrap_or_else(|| panic!("serviced stretch must render again"))
    });
    permit_alloc(|| {
        effect.prepare(spec);
        drop(second_output);
    });

    loop {
        let terminal = assert_no_alloc(|| effect.drain(8_192).expect("prepared terminal drain"));
        let finished = terminal.is_none();
        permit_alloc(|| {
            effect.prepare(spec);
            drop(terminal);
        });
        if finished {
            break;
        }
    }
}

#[kithara::test]
#[case(StretchKind::Signalsmith)]
#[cfg_attr(
    all(
        not(target_os = "android"),
        not(all(target_os = "windows", target_env = "msvc"))
    ),
    case(StretchKind::Bungee)
)]
fn timestretch_pending_and_maximum_output_are_allocation_free(
    allocation_ramp: Vec<f32>,
    #[case] kind: StretchKind,
) {
    const FRAMES: usize = 8_192;
    let pools = make_pools();
    let spec = AudioSpec::new(2, NonZeroU32::new(44_100).expect("test rate"));
    let (mut maximum, input) = permit_alloc(|| {
        let mut maximum = warp_renderer(0.05, kind, spec, pools.clone());
        maximum.prepare(spec);
        let input = make_chunk(&pools, FRAMES, 2, &allocation_ramp);
        (maximum, input)
    });
    let maximum_output = assert_no_alloc(|| {
        maximum
            .render(input)
            .continue_value()
            .expect("prepared source must be admitted")
            .unwrap_or_else(|| panic!("maximum prepared output must render"))
    });
    assert_eq!(maximum_output.frames(), 163_840);
    permit_alloc(|| {
        maximum.prepare(spec);
        drop(maximum_output);
    });

    let (mut pending, input) = permit_alloc(|| {
        let mut pending = warp_renderer(2.0, kind, spec, pools.clone());
        pending.prepare(spec);
        let input = make_chunk(&pools, 1, 2, &allocation_ramp);
        (pending, input)
    });
    assert_no_alloc(|| {
        assert!(matches!(
            pending.render(input),
            std::ops::ControlFlow::Continue(None)
        ));
    });
    permit_alloc(|| pending.prepare(spec));

    let terminal = assert_no_alloc(|| {
        pending
            .drain(8_192)
            .expect("prepared terminal drain")
            .unwrap_or_else(|| panic!("pending frame plus terminal tail must render"))
    });
    permit_alloc(|| {
        pending.prepare(spec);
        drop(terminal);
    });
}
