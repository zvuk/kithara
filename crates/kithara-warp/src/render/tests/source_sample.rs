use std::num::{NonZeroU32, NonZeroU128, NonZeroUsize};

use kithara_signal::{AudioChunk, AudioChunkInfo, AudioSpec};
use kithara_stretch::ElasticError;
use kithara_sync::SpeedCurve;
use kithara_test_utils::kithara;
use num_traits::ToPrimitive;

use super::{SourceResidency, source_frame};
use crate::{
    StretchKind, Warp, WarpConfig,
    test_pools::{pools, sample_buffer},
};

fn reference(
    samples: &[f32],
    channels: usize,
    source: usize,
    fraction: f64,
    speed: f64,
) -> Vec<f32> {
    let last = samples.len() / channels - 1;
    (0..channels)
        .map(|channel| {
            let current = samples[source * channels + channel];
            if speed <= 1.0 {
                if fraction == 0.0 {
                    return current;
                }
                let next = samples[(source + 1).min(last) * channels + channel];
                return (f64::from(next) - f64::from(current))
                    .mul_add(fraction, f64::from(current))
                    .to_f32()
                    .expect("float conversion");
            }
            let mut total = 0.0;
            let mut weights = 0.0;
            for offset in -16_i32..=16 {
                let distance = f64::from(offset) - fraction;
                let angle = std::f64::consts::PI * distance * speed.recip();
                let sinc = if angle == 0.0 {
                    speed.recip()
                } else {
                    angle.sin() / (std::f64::consts::PI * distance)
                };
                let weight = sinc * ((1.0 + (std::f64::consts::PI * distance / 17.0).cos()) * 0.5);
                let frame = (i32::try_from(source).expect("fixture frame") + offset)
                    .clamp(0, i32::try_from(last).expect("last frame"));
                total += f64::from(
                    samples[usize::try_from(frame).expect("clamped") * channels + channel],
                ) * weight;
                weights += weight;
            }
            (total / weights).to_f32().expect("float conversion")
        })
        .collect()
}

#[kithara::test]
fn source_window_matches_independent_scalar_rows() {
    let pools = pools();
    let denominator = NonZeroU128::new(1 << 24).expect("exact phase clock");
    for channels in [1_u16, 2, 4, 6, 9] {
        let count = usize::from(channels);
        let samples: Vec<f32> = (0..64 * count)
            .map(|index| {
                let value = i32::try_from(index % 37).expect("bounded PCM") - 18;
                value.to_f32().expect("PCM") / 32.0
            })
            .collect();
        let mut resident = SourceResidency::prepare(&pools, None, 0, 64, 0, count)
            .expect("bounded channel history");
        resident
            .append(
                AudioChunkInfo {
                    spec: AudioSpec::new(channels, NonZeroU32::new(48_000).expect("sample rate")),
                    frames: 64,
                    ..AudioChunkInfo::default()
                },
                &samples,
            )
            .expect("resident PCM");
        for source in [0_usize, 1, 16, 47, 63] {
            for phase in [0_u128, 1, 1 << 23, (1 << 24) - 1] {
                let fraction =
                    phase.to_f64().expect("phase") / denominator.get().to_f64().expect("clock");
                for speed in [0.25, 1.0, 1.000_001, 1.5, 4.0] {
                    let mut output = vec![7.0; count + 1];
                    source_frame(
                        &resident,
                        (
                            u128::try_from(source).expect("frame") * denominator.get() + phase,
                            denominator,
                        ),
                        speed,
                        Some(64),
                        &mut output[..count],
                    )
                    .expect("clamped source window");
                    let expected = reference(&samples, count, source, fraction, speed);
                    assert_eq!(
                        output[..count]
                            .iter()
                            .map(|x| x.to_bits())
                            .collect::<Vec<_>>(),
                        expected.iter().map(|x| x.to_bits()).collect::<Vec<_>>(),
                        "channels {channels}, source {source}, phase {phase}, speed {speed}"
                    );
                    assert_eq!(output[count], 7.0);
                }
            }
        }
    }
}

#[kithara::test]
fn integer_phase_needs_no_next_frame_and_preserves_special_bits() {
    let pools = pools();
    let samples = [
        -0.0,
        f32::from_bits(1),
        f32::NAN,
        f32::INFINITY,
        f32::NEG_INFINITY,
        0.0,
    ];
    let mut resident = SourceResidency::prepare(&pools, None, 0, 1, 0, 6).expect("single frame");
    resident
        .append(
            AudioChunkInfo {
                spec: AudioSpec::new(6, NonZeroU32::new(48_000).expect("sample rate")),
                frames: 1,
                ..AudioChunkInfo::default()
            },
            &samples,
        )
        .expect("special PCM");
    let mut output = [7.0; 6];
    source_frame(&resident, (0, NonZeroU128::MIN), 1.0, None, &mut output)
        .expect("no lookahead required");
    assert_eq!(output.map(f32::to_bits), samples.map(f32::to_bits));
    for speed in [1.0, 2.0] {
        output.fill(7.0);
        assert_eq!(
            source_frame(
                &resident,
                (1, NonZeroU128::new(2).expect("phase")),
                speed,
                None,
                &mut output
            ),
            Err(ElasticError::EnginePreparation(
                "required source is not resident"
            ))
        );
        assert_eq!(output, [7.0; 6], "tap resolution precedes output writes");
    }
    output.fill(7.0);
    assert_eq!(
        source_frame(
            &resident,
            (u128::MAX, NonZeroU128::MIN),
            1.0,
            None,
            &mut output
        ),
        Err(ElasticError::SampleCountOverflow)
    );
    assert_eq!(output, [7.0; 6]);
    resident.origin = None;
    assert_eq!(
        source_frame(&resident, (0, NonZeroU128::MIN), 1.0, None, &mut output),
        Err(ElasticError::EmptySource)
    );
    assert_eq!(output, [7.0; 6]);
}

#[kithara::test]
fn renderer_pcm_matches_independent_rows_across_controls_and_budgets() {
    for channels in [1_u16, 2, 4, 6] {
        let count = usize::from(channels);
        let samples: Vec<f32> = (0..2048 * count)
            .map(|index| {
                let value = i32::try_from(index % 37).expect("bounded PCM") - 18;
                value.to_f32().expect("PCM") / 32.0
            })
            .collect();
        let spec = AudioSpec::new(channels, NonZeroU32::new(48_000).expect("sample rate"));
        for budgets in [[32_usize; 4], [1, 7, 127, 9]] {
            let pools = pools();
            let config = WarpConfig::builder()
                .backend(StretchKind::Glide)
                .keylock(false)
                .speed(0.75)
                .source_block_frames(NonZeroUsize::new(512).expect("fixture source block"))
                .render_quantum_frames(NonZeroUsize::new(127).expect("quantum"))
                .build();
            let mut renderer = Warp::new((), &config).renderer(spec, pools.clone());
            let mut revision = 0;
            for origin in [0_usize, 64] {
                if origin != 0 {
                    renderer.reset();
                }
                let mut decoded = origin;
                let mut cursor = u128::try_from(origin).expect("origin") * 4;
                for (budget, step) in budgets.into_iter().zip([3_u128, 6, 2, 5]) {
                    revision += 1;
                    let speed = step.to_f64().expect("quarter-frame step") / 4.0;
                    renderer
                        .set_speed(
                            SpeedCurve::Constant(speed.to_f32().expect("speed")),
                            revision,
                        )
                        .expect("live constant rate");
                    renderer
                        .prepare_engine_latency(spec)
                        .expect("renderer service");
                    let meta = AudioChunkInfo {
                        spec,
                        frame_offset: u64::try_from(decoded).expect("decode frontier"),
                        ..AudioChunkInfo::default()
                    };
                    let frames = renderer
                        .prepare_source_window(meta, 2048 - decoded, budget)
                        .expect("physical source demand")
                        .get();
                    let input = AudioChunk::new(
                        AudioChunkInfo {
                            frames: u32::try_from(frames).expect("physical frames"),
                            ..meta
                        },
                        sample_buffer(
                            &pools,
                            &samples[decoded * count..(decoded + frames) * count],
                        ),
                    );
                    decoded += frames;
                    let output = renderer
                        .render_quantum(input)
                        .continue_value()
                        .expect("matching prepared source")
                        .expect("mapped output");
                    assert_eq!(output.frames(), budget);
                    let span = output.meta.source_span.expect("exact output geometry");
                    for (frame, actual) in output.samples.chunks_exact(count).enumerate() {
                        let position = cursor + u128::try_from(frame).expect("output frame") * step;
                        let (numerator, denominator) = span
                            .source_ratio_at(u64::try_from(frame).expect("output frame"))
                            .expect("mapped source coordinate");
                        assert_eq!(numerator * 4, position * denominator.get());
                        let source = usize::try_from(position / 4).expect("source frame") - origin;
                        let fraction = (position % 4).to_f64().expect("exact phase") / 4.0;
                        let expected =
                            reference(&samples[origin * count..], count, source, fraction, speed);
                        assert_eq!(
                            actual
                                .iter()
                                .map(|value| value.to_bits())
                                .collect::<Vec<_>>(),
                            expected
                                .iter()
                                .map(|value| value.to_bits())
                                .collect::<Vec<_>>(),
                            "channels {channels}, origin {origin}, budget {budget}, frame {frame}, step {step}"
                        );
                    }
                    cursor += u128::try_from(budget).expect("output budget") * step;
                }
            }
        }
    }
}
