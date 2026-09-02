use std::num::NonZero;

use kithara_platform::{sync::Arc, time::Duration};
use kithara_signal::{AudioChunk, AudioChunkInfo, AudioSpec};
use realfft::RealFftPlanner;

use super::{StretchControls, WarpRenderer as GenericWarpRenderer};
use crate::{
    RenderPublisher, Warp, WarpConfig,
    test_pools::{Pools, TestPools, pools, sample_buffer},
};

type WarpRenderer = GenericWarpRenderer<TestPools>;

mod playback;
mod target;
mod timeline;

struct Consts;

impl Consts {
    const CH: u16 = 2;
    const F0: f64 = 440.0;
    /// FFT length for the pitch (dominant-frequency) check.
    const N: usize = 1 << 14;
    const SR: u32 = 44_100;
}

fn f32_of(x: f64) -> f32 {
    num_traits::cast(x).unwrap_or_default()
}

fn f64_of(x: usize) -> f64 {
    num_traits::cast(x).unwrap_or_default()
}

/// Interleaved stereo sine at `F0`, phase-accumulated to avoid drift.
fn sine(frames: usize) -> Vec<f32> {
    let inc = std::f64::consts::TAU * Consts::F0 / f64::from(Consts::SR);
    let mut phase = 0.0_f64;
    let mut out = Vec::with_capacity(frames * usize::from(Consts::CH));
    for _ in 0..frames {
        let s = f32_of(0.5 * phase.sin());
        out.push(s);
        out.push(s);
        phase += inc;
    }
    out
}

fn chunk(pools: &Pools, samples: &[f32]) -> AudioChunk {
    let frames = samples.len() / usize::from(Consts::CH);
    AudioChunk::new(
        AudioChunkInfo {
            spec: AudioSpec {
                channels: Consts::CH,
                sample_rate: NonZero::new(Consts::SR).unwrap(),
            },
            frames: u32::try_from(frames).unwrap_or(0),
            timestamp: Duration::ZERO,
            ..Default::default()
        },
        sample_buffer(pools, samples),
    )
}

fn chunk_at(pools: &Pools, samples: &[f32], frame_offset: u64) -> AudioChunk {
    let mut chunk = chunk(pools, samples);
    let frame_end = frame_offset
        .checked_add(u64::try_from(chunk.frames()).expect("test frame count fits"))
        .expect("test frame end fits");
    chunk.meta.frame_offset = frame_offset;
    chunk.meta.timestamp = spec()
        .duration_for(frame_offset)
        .expect("test timestamp fits");
    chunk.meta.end_timestamp = spec()
        .duration_for(frame_end)
        .expect("test end timestamp fits");
    chunk
}

/// Index of the strongest spectral bin (skipping DC) of a mono window
/// taken from the middle of `mono`.
fn dominant_bin(mono: &[f32]) -> usize {
    let start = (mono.len().saturating_sub(Consts::N)) / 2;
    let seg = &mono[start..start + Consts::N];
    let mut planner = RealFftPlanner::<f32>::new();
    let fft = planner.plan_fft_forward(Consts::N);
    let mut input = fft.make_input_vec();
    input.copy_from_slice(seg);
    let mut spectrum = fft.make_output_vec();
    fft.process(&mut input, &mut spectrum).unwrap();
    spectrum
        .iter()
        .enumerate()
        .skip(1)
        .max_by(|a, b| a.1.norm().total_cmp(&b.1.norm()))
        .map_or(0, |(i, _)| i)
}

fn expected_bin(freq: f64) -> usize {
    num_traits::cast((freq * f64_of(Consts::N) / f64::from(Consts::SR)).round()).unwrap_or(0)
}

fn spec() -> AudioSpec {
    AudioSpec {
        channels: Consts::CH,
        sample_rate: NonZero::new(Consts::SR).unwrap(),
    }
}

fn renderer(controls: Arc<StretchControls>) -> WarpRenderer {
    renderer_with_publisher(controls).1
}

fn renderer_with_publisher(controls: Arc<StretchControls>) -> (RenderPublisher, WarpRenderer) {
    let config = WarpConfig::builder()
        .stretch(controls)
        .render_quantum_frames(
            NonZero::new(WarpRenderer::DIRECT_OUTPUT_FRAME_LIMIT)
                .expect("renderer output limit is non-zero"),
        )
        .build();
    let warp = Warp::new((), &config);
    let publisher = warp.publisher();
    (publisher, warp.renderer(spec(), pools()))
}

fn render_serviced(fx: &mut WarpRenderer, input: AudioChunk) -> Option<AudioChunk> {
    fx.prepare(spec());
    let output = fx.render(input);
    fx.prepare(spec());
    output
}

fn flush_serviced(fx: &mut WarpRenderer) -> Option<AudioChunk> {
    fx.prepare(spec());
    let output = fx.flush();
    fx.prepare(spec());
    output
}
