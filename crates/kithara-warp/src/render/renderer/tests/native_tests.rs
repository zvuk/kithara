use std::num::NonZeroUsize;

use kithara_test_utils::kithara;
use num_traits::ToPrimitive;

use super::*;
use crate::{
    Pitch, Warp,
    test_pools::{pools, sample_buffer},
};

const SAMPLE_RATE: u32 = 8_000;
const WARMUP_FRAMES: usize = 4_096;
const CAPTURE_FRAMES: usize = 2_000;

#[kithara::test]
#[kithara::hang_watchdog]
#[case::single_frame(1)]
#[case::block(128)]
fn native_keylock_preserves_the_physical_source_frequency(#[case] quantum: usize) {
    let spec = AudioSpec::new(
        consts::CH,
        NonZeroU32::new(SAMPLE_RATE).expect("sample rate"),
    );
    let config = WarpConfig::builder()
        .backend(StretchKind::Signalsmith)
        .speed(0.8)
        .pitch(Pitch::HELD)
        .render_quantum_frames(NonZeroUsize::new(128).expect("quantum"))
        .build();
    let mut renderer = Warp::new((), &config).renderer(spec, pools());
    let tone = kithara_test_fixtures::unit_fixtures::warp_native_tone();
    renderer.prepare_engine_latency(spec).expect("service");
    let rendered_bound = (WARMUP_FRAMES + CAPTURE_FRAMES).next_multiple_of(128);
    let lookahead = renderer
        .projection_lookahead()
        .expect("projection lookahead");
    assert!(rendered_bound + lookahead <= tone.len() / 2);
    let mut source = 0;
    let mut capture = Vec::new();
    while capture.len() < WARMUP_FRAMES + CAPTURE_FRAMES {
        hang_tick!();
        let quantum = if capture.len() < WARMUP_FRAMES {
            128
        } else {
            quantum
        };
        renderer.prepare_engine_latency(spec).expect("service");
        let frames = renderer
            .prepare_source_window(
                AudioChunkInfo {
                    spec,
                    frame_offset: source,
                    ..Default::default()
                },
                65_536,
                quantum,
            )
            .expect("source quantum")
            .get();
        let start = usize::try_from(source).expect("source frame") * 2;
        let samples = &tone[start..start + frames * 2];
        let output = renderer
            .render_quantum(AudioChunk::new(
                AudioChunkInfo {
                    spec,
                    frame_offset: source,
                    frames: u32::try_from(frames).expect("frames"),
                    ..Default::default()
                },
                sample_buffer(&renderer.pools, samples),
            ))
            .continue_value()
            .expect("prepared shape")
            .expect("continuous audio");
        assert_eq!(output.frames(), quantum);
        capture.extend(
            output
                .samples
                .as_chunks::<2>()
                .0
                .iter()
                .map(|frame| frame[0]),
        );
        source += u64::try_from(frames).expect("source frames");
        hang_reset!();
    }
    let capture = &capture[WARMUP_FRAMES..WARMUP_FRAMES + CAPTURE_FRAMES];
    let mut sine = 0.0;
    let mut cosine = 0.0;
    let mut energy = 0.0;
    for (frame, sample) in capture.iter().enumerate() {
        let phase =
            frame.to_f64().expect("frame") * std::f64::consts::TAU * 440.0 / f64::from(SAMPLE_RATE);
        sine += f64::from(*sample) * phase.sin();
        cosine += f64::from(*sample) * phase.cos();
        energy += f64::from(*sample).powi(2);
    }
    let count = capture.len().to_f64().expect("sample count");
    let amplitude = 2.0 * sine.hypot(cosine) / count;
    let residual = (1.0 - 2.0 * (sine * sine + cosine * cosine) / count / energy)
        .max(0.0)
        .sqrt();
    assert!((0.47..=0.51).contains(&amplitude), "amplitude={amplitude}");
    assert!(residual <= 0.01, "440 Hz residual={residual}");
}
