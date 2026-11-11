use std::num::NonZeroUsize;

use kithara_test_utils::kithara;

use super::*;
use crate::{
    Warp, WarpRenderError,
    test_pools::{TestPools, pools, sample_buffer},
};

type Renderer = WarpRenderer<TestPools>;

const QUANTUM: usize = 128;
const GAP_FRAMES: u64 = 48_000;

fn keylocked_renderer() -> Renderer {
    let spec = AudioSpec::new(
        consts::CH,
        NonZeroU32::new(consts::SR).expect("sample rate"),
    );
    let config = WarpConfig::builder()
        .backend(StretchKind::Signalsmith)
        .speed(0.5)
        .keylock(true)
        .render_quantum_frames(NonZeroUsize::new(QUANTUM).expect("quantum"))
        .build();
    Warp::new((), &config).renderer(spec, pools())
}

fn tone(renderer: &Renderer, source: u64, frames: usize) -> AudioChunk {
    let channels = usize::from(renderer.spec.channels);
    let rate = f64::from(renderer.spec.sample_rate.get());
    let samples: Vec<f32> = (source..)
        .take(frames)
        .flat_map(|frame| {
            let position: f64 = num_traits::cast(frame).unwrap_or_default();
            let value: f32 =
                num_traits::cast((position / rate * 440.0 * std::f64::consts::TAU).sin() * 0.5)
                    .unwrap_or_default();
            std::iter::repeat_n(value, channels)
        })
        .collect();
    AudioChunk::new(
        AudioChunkInfo {
            spec: renderer.spec,
            frame_offset: source,
            frames: u32::try_from(frames).expect("tone frames"),
            ..Default::default()
        },
        sample_buffer(&renderer.pools, &samples),
    )
}

fn service(renderer: &mut Renderer) {
    let spec = renderer.spec;
    renderer.prepare(spec);
    if renderer.transition_pending() {
        match renderer.prepare_engine_latency(spec) {
            Ok(_) | Err(WarpRenderError::NeedsService) => {}
            Err(error) => panic!("engine latency: {error}"),
        }
    }
}

/// One render-lane quantum: service, prepare the source window, and on
/// `NeedsService` service and prepare again before rendering.
fn tick(renderer: &mut Renderer, source: &mut u64) -> Vec<f32> {
    let meta = AudioChunkInfo {
        spec: renderer.spec,
        frame_offset: *source,
        ..Default::default()
    };
    service(renderer);
    let frames = match renderer.prepare_source_window(meta, 65_536, QUANTUM) {
        Err(WarpRenderError::NeedsService) => {
            service(renderer);
            renderer.prepare_source_window(meta, 65_536, QUANTUM)
        }
        prepared => prepared,
    }
    .expect("source window")
    .get();
    let chunk = tone(renderer, *source, frames);
    *source += u64::try_from(frames).expect("source frames");
    renderer
        .render_quantum(chunk)
        .continue_value()
        .expect("rendered quantum")
        .map(|output| {
            output
                .samples
                .chunks_exact(usize::from(output.meta.spec.channels))
                .map(|frame| frame[0])
                .collect()
        })
        .unwrap_or_default()
}

fn after_gap(announced: bool) -> Vec<Vec<f32>> {
    let mut renderer = keylocked_renderer();
    let mut source = 0;
    for _ in 0..64 {
        tick(&mut renderer, &mut source);
    }
    assert!(renderer.active, "fixture: the keylock engine is running");
    source += GAP_FRAMES;
    if announced {
        renderer.reset();
    }
    (0..64).map(|_| tick(&mut renderer, &mut source)).collect()
}

#[kithara::test]
fn a_detected_source_gap_renders_as_an_announced_reset() {
    let announced = after_gap(true);
    let detected = after_gap(false);
    assert!(
        announced.iter().flatten().any(|sample| sample.abs() > 0.1),
        "fixture: the announced reset renders the tone"
    );
    let first_difference = announced
        .iter()
        .zip(&detected)
        .position(|(left, right)| left != right);
    assert_eq!(
        first_difference, None,
        "quanta after the gap differ from the announced reset"
    );
}
