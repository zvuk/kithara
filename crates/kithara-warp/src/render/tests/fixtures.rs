use std::{num::NonZero, ops::ControlFlow};

use kithara_platform::time::Duration;
use kithara_signal::{AudioChunk, AudioChunkInfo, AudioSpec};
#[cfg(any(
    feature = "stretch-signalsmith",
    feature = "stretch-bungee",
    feature = "stretch-glide"
))]
use realfft::RealFftPlanner;

use super::super::WarpRenderer as GenericWarpRenderer;
use crate::{
    Warp, WarpConfig, consts,
    test_pools::{Pools, TestPools, pools, sample_buffer},
};

pub(in crate::render) type WarpRenderer = GenericWarpRenderer<TestPools>;

pub(super) trait TerminalDrain {
    fn flush(&mut self) -> Option<AudioChunk>;
}

impl TerminalDrain for WarpRenderer {
    fn flush(&mut self) -> Option<AudioChunk> {
        self.drain(usize::MAX).expect("terminal drain")
    }
}

pub(super) fn f64_of(x: usize) -> f64 {
    num_traits::cast(x).unwrap_or_default()
}

pub(super) fn chunk(pools: &Pools, samples: &[f32]) -> AudioChunk {
    let frames = samples.len() / usize::from(consts::CH);
    AudioChunk::new(
        AudioChunkInfo {
            spec: AudioSpec {
                channels: consts::CH,
                sample_rate: NonZero::new(consts::SR).unwrap(),
            },
            frames: u32::try_from(frames).unwrap_or(0),
            timestamp: Duration::ZERO,
            ..Default::default()
        },
        sample_buffer(pools, samples),
    )
}

/// Index of the strongest spectral bin (skipping DC) of a mono window
/// taken from the middle of `mono`.
#[cfg(any(
    feature = "stretch-signalsmith",
    feature = "stretch-bungee",
    feature = "stretch-glide"
))]
pub(super) fn dominant_bin(mono: &[f32]) -> usize {
    let start = (mono.len().saturating_sub(consts::N)) / 2;
    let seg = &mono[start..start + consts::N];
    let mut planner = RealFftPlanner::<f32>::new();
    let fft = planner.plan_fft_forward(consts::N);
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

#[cfg(any(
    feature = "stretch-signalsmith",
    feature = "stretch-bungee",
    feature = "stretch-glide"
))]
pub(super) fn expected_bin(freq: f64) -> usize {
    num_traits::cast((freq * f64_of(consts::N) / f64::from(consts::SR)).round()).unwrap_or(0)
}

pub(super) fn spec() -> AudioSpec {
    AudioSpec {
        channels: consts::CH,
        sample_rate: NonZero::new(consts::SR).unwrap(),
    }
}

pub(super) fn renderer(config: &WarpConfig) -> WarpRenderer {
    Warp::new((), config).renderer(spec(), pools())
}

#[cfg(any(
    feature = "stretch-signalsmith",
    feature = "stretch-bungee",
    feature = "stretch-glide"
))]
pub(in crate::render) fn flush_serviced(fx: &mut WarpRenderer) -> Option<AudioChunk> {
    fx.prepare(spec());
    let output = fx.flush();
    fx.prepare(spec());
    output
}

const LANE_STEPS: usize = 1 << 16;

/// Decoded source held between quanta and rendered the way the render lane
/// renders it: service, prepare the window from the held source, supply
/// exactly the prepared frames, and keep the unconsumed suffix.
#[derive(Default)]
pub(in crate::render) struct LaneFeed {
    held: Vec<f32>,
    meta: Option<AudioChunkInfo>,
}

impl LaneFeed {
    pub(in crate::render) fn feed(
        &mut self,
        fx: &mut WarpRenderer,
        chunk: AudioChunk,
    ) -> Vec<AudioChunk> {
        let mut out = Vec::new();
        if let Some(meta) = self.meta.as_mut() {
            let held = u64::try_from(self.held.len() / usize::from(consts::CH))
                .expect("held frames fit u64");
            assert_eq!(
                chunk.meta.frame_offset,
                meta.frame_offset + held,
                "decoded source is contiguous"
            );
            meta.source_span = meta.source_span.map(|span| {
                chunk
                    .meta
                    .source_span
                    .and_then(|next| span.followed_by(next))
                    .expect("held source mappings join")
            });
        } else {
            service(fx, &mut out);
            if prepare_window(fx, chunk.meta, chunk.frames(), &mut out) == chunk.frames() {
                render_quantum(fx, chunk, &mut out);
                fx.prepare(spec());
                return out;
            }
            self.meta = Some(chunk.meta);
        }
        self.held.extend_from_slice(&chunk.samples);
        drop(chunk);
        for _ in 0..LANE_STEPS {
            service(fx, &mut out);
            let held = self.held_frames();
            let Some(meta) = self.window_meta(held) else {
                fx.prepare(spec());
                return out;
            };
            let frames = prepare_window(fx, meta, held, &mut out);
            if frames > held {
                fx.prepare(spec());
                return out;
            }
            self.render_held(fx, meta, frames, &mut out);
        }
        panic!("held source must drain into quanta");
    }

    pub(in crate::render) fn end(&mut self, fx: &mut WarpRenderer) -> Vec<AudioChunk> {
        let mut out = Vec::new();
        let held = self.held_frames();
        let Some(meta) = self.window_meta(held) else {
            return out;
        };
        service(fx, &mut out);
        assert!(
            prepare_window(fx, meta, held, &mut out) > held,
            "a covered window renders before the end of source"
        );
        let frames = fx
            .prepare_terminal_quantum(held)
            .expect("terminal quantum")
            .get();
        self.render_held(fx, meta, frames, &mut out);
        fx.prepare(spec());
        out
    }

    fn held_frames(&self) -> usize {
        self.held.len() / usize::from(consts::CH)
    }

    fn window_meta(&mut self, held: usize) -> Option<AudioChunkInfo> {
        if held == 0 {
            self.meta = None;
        }
        self.meta.map(|meta| AudioChunkInfo {
            frames: u32::try_from(held).expect("held frames fit u32"),
            ..meta
        })
    }

    fn render_held(
        &mut self,
        fx: &mut WarpRenderer,
        meta: AudioChunkInfo,
        frames: usize,
        out: &mut Vec<AudioChunk>,
    ) {
        let samples = frames * usize::from(consts::CH);
        let consumed = u64::try_from(frames).expect("frames fit u64");
        let held = u64::from(meta.frames);
        let span = |range| {
            meta.source_span
                .map(|span| span.for_output_range(range).expect("held source mapping"))
        };
        let input = AudioChunk::new(
            AudioChunkInfo {
                frames: u32::try_from(frames).expect("quantum frames fit u32"),
                source_span: span(0..consumed),
                ..meta
            },
            sample_buffer(&fx.pools, &self.held[..samples]),
        );
        self.held.drain(..samples);
        self.meta = (consumed < held).then(|| AudioChunkInfo {
            frame_offset: meta.frame_offset + consumed,
            source_span: span(consumed..held),
            ..meta
        });
        render_quantum(fx, input, out);
    }
}

pub(in crate::render) fn drain_tail(fx: &mut WarpRenderer) -> Vec<AudioChunk> {
    let mut out = Vec::new();
    for _ in 0..LANE_STEPS {
        match flush_serviced(fx) {
            Some(output) => out.push(output),
            None => return out,
        }
    }
    panic!("terminal drain must converge");
}

pub(in crate::render) fn samples(outputs: &[AudioChunk]) -> Vec<f32> {
    outputs
        .iter()
        .flat_map(|output| output.samples.iter().copied())
        .collect()
}

fn service(fx: &mut WarpRenderer, out: &mut Vec<AudioChunk>) {
    for _ in 0..LANE_STEPS {
        fx.prepare(spec());
        if fx.transition_pending() {
            match fx.prepare_engine_latency(spec()) {
                Ok(_) | Err(crate::WarpRenderError::NeedsService) => {}
                Err(error) => panic!("renderer service: {error}"),
            }
        }
        if !fx.transition_pending() {
            return;
        }
        if let Some(output) = fx.drain(usize::MAX).expect("live transition drain") {
            out.push(output);
        }
    }
    panic!("a live transition must drain");
}

fn prepare_window(
    fx: &mut WarpRenderer,
    meta: AudioChunkInfo,
    held: usize,
    out: &mut Vec<AudioChunk>,
) -> usize {
    for _ in 0..LANE_STEPS {
        match fx.prepare_source_window(meta, held, usize::MAX) {
            Ok(frames) => return frames.get(),
            Err(crate::WarpRenderError::NeedsService) => service(fx, out),
            Err(error) => panic!("source window at {}: {error}", meta.frame_offset),
        }
    }
    panic!("a source window must become plannable");
}

fn render_quantum(fx: &mut WarpRenderer, input: AudioChunk, out: &mut Vec<AudioChunk>) {
    match fx.render_quantum(input) {
        ControlFlow::Continue(output) => out.extend(output),
        ControlFlow::Break(input) => panic!("prepared quantum refused {:?}", input.meta),
    }
}
