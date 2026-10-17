use kithara_stretch::ElasticError;
use num_traits::ToPrimitive;

use super::renderer::residency::SourceResidency;

pub(super) fn source_frame(
    resident: &SourceResidency,
    position: (u128, std::num::NonZeroU128),
    speed: f64,
    terminal: Option<u64>,
    output: &mut [f32],
) -> Result<(), ElasticError> {
    let channels = output.len();
    let (numerator, denominator) = position;
    let denominator = denominator.get();
    let source =
        i64::try_from(numerator / denominator).map_err(|_| ElasticError::SampleCountOverflow)?;
    let fraction = (numerator % denominator)
        .to_f64()
        .ok_or(ElasticError::SampleCountOverflow)?
        / denominator
            .to_f64()
            .ok_or(ElasticError::SampleCountOverflow)?;
    let first = i64::try_from(resident.origin.ok_or(ElasticError::EmptySource)?)
        .map_err(|_| ElasticError::SampleCountOverflow)?;
    let last = terminal
        .map(|end| i64::try_from(end.saturating_sub(1)))
        .transpose()
        .map_err(|_| ElasticError::SampleCountOverflow)?;
    let sample_offset = |offset: i64| {
        let frame = source
            .checked_add(offset)
            .ok_or(ElasticError::SampleCountOverflow)?;
        let frame = last.map_or_else(|| frame.max(first), |last| frame.clamp(first, last));
        let end = u64::try_from(frame)
            .ok()
            .and_then(|frame| frame.checked_add(1))
            .ok_or(ElasticError::SampleCountOverflow)?;
        Ok::<_, ElasticError>(resident.range(frame, end, channels)?.start)
    };
    if speed <= 1.0 {
        return interpolate_source_frame(resident, sample_offset, fraction, output);
    }
    downsample_source_frame(resident, sample_offset, fraction, speed, output)
}

fn interpolate_source_frame(
    resident: &SourceResidency,
    sample_offset: impl Fn(i64) -> Result<usize, ElasticError>,
    fraction: f64,
    output: &mut [f32],
) -> Result<(), ElasticError> {
    let channels = output.len();
    let current = sample_offset(0)?;
    if fraction == 0.0 {
        output.copy_from_slice(&resident.samples[current..current + channels]);
        return Ok(());
    }
    let next = sample_offset(1)?;
    for (channel, sample) in output.iter_mut().enumerate() {
        let current = f64::from(resident.samples[current + channel]);
        let next = f64::from(resident.samples[next + channel]);
        *sample = (next - current)
            .mul_add(fraction, current)
            .to_f32()
            .ok_or(ElasticError::SampleCountOverflow)?;
    }
    Ok(())
}

fn downsample_source_frame(
    resident: &SourceResidency,
    sample_offset: impl Fn(i64) -> Result<usize, ElasticError>,
    fraction: f64,
    speed: f64,
    output: &mut [f32],
) -> Result<(), ElasticError> {
    let radius = i64::try_from(crate::consts::SOURCE_RADIUS)
        .map_err(|_| ElasticError::SampleCountOverflow)?;
    let cutoff = speed.recip();
    let mut taps = [(0, 0.0); 2 * crate::consts::SOURCE_RADIUS + 1];
    let mut weights = 0.0;
    for (tap, offset) in taps.iter_mut().zip(-radius..=radius) {
        let distance = offset.to_f64().ok_or(ElasticError::SampleCountOverflow)? - fraction;
        let weight = kithara_dsp::interp::sinc_hann_weight(
            distance,
            cutoff,
            (radius + 1)
                .to_f64()
                .ok_or(ElasticError::SampleCountOverflow)?,
        );
        *tap = (sample_offset(offset)?, weight);
        weights += weight;
    }
    for (channel, sample) in output.iter_mut().enumerate() {
        let mut total = 0.0;
        for &(offset, weight) in &taps {
            total += f64::from(resident.samples[offset + channel]) * weight;
        }
        *sample = (total / weights)
            .to_f32()
            .ok_or(ElasticError::SampleCountOverflow)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::num::{NonZeroU32, NonZeroU128};

    use kithara_signal::{AudioChunkInfo, AudioSpec};
    use kithara_test_utils::kithara;

    use super::*;
    use crate::test_pools::pools;

    #[kithara::test]
    fn downsampling_keeps_the_mapped_source_coordinate() {
        let pools = pools();
        let mut resident =
            SourceResidency::prepare(&pools, None, 32, 64, 0, 1).expect("retained source history");
        let samples: Vec<_> = (0..64_u16).map(|frame| f32::from(frame) / 64.0).collect();
        resident
            .append(
                AudioChunkInfo {
                    spec: AudioSpec::new(1, NonZeroU32::new(44_100).expect("sample rate")),
                    frames: 64,
                    ..AudioChunkInfo::default()
                },
                &samples,
            )
            .expect("exact mapped source span");
        let mut output = [0.0];
        source_frame(&resident, (16, NonZeroU128::MIN), 4.0, None, &mut output)
            .expect("all filter taps are in the resident source window");
        let actual = output[0];
        assert_eq!(actual, 0.25);
        for channel_count in [2, 4] {
            let channels = usize::from(channel_count);
            let mut resident = SourceResidency::prepare(&pools, None, 32, 64, 0, channels)
                .expect("independent channel history");
            let samples: Vec<_> = (0..64_u16)
                .flat_map(|frame| {
                    let value = f32::from(frame) / 64.0;
                    [value, -value, 0.5, -0.75].into_iter().take(channels)
                })
                .collect();
            resident
                .append(
                    AudioChunkInfo {
                        spec: AudioSpec::new(
                            channel_count,
                            NonZeroU32::new(44_100).expect("sample rate"),
                        ),
                        frames: 64,
                        ..AudioChunkInfo::default()
                    },
                    &samples,
                )
                .expect("distinct interleaved channels");
            for speed in [0.25, 1.0, 4.0] {
                for position in [
                    (16, NonZeroU128::MIN),
                    (65, NonZeroU128::new(4).expect("phase")),
                ] {
                    let mut output = [0.0; 4];
                    source_frame(&resident, position, speed, None, &mut output[..channels])
                        .expect("one mapped frame across independent channels");
                    assert!((0.25..=17.0 / 64.0).contains(&output[0]));
                    assert_eq!(output[1].to_bits(), (-output[0]).to_bits());
                    if channels == 4 {
                        assert_eq!(output[2], 0.5);
                        assert_eq!(output[3], -0.75);
                    }
                }
            }
        }
    }

    #[kithara::test]
    fn quadratic_sampling_keeps_the_fraction_when_the_window_position_rounds_up() {
        let pools = pools();
        let mut resident =
            SourceResidency::prepare(&pools, None, 0, 3, 0, 1).expect("three resident frames");
        resident
            .append(
                AudioChunkInfo {
                    spec: AudioSpec::new(1, NonZeroU32::new(44_100).expect("sample rate")),
                    frames: 3,
                    ..AudioChunkInfo::default()
                },
                &[1.0, 1.0, -1.0],
            )
            .expect("source window");
        let denominator = NonZeroU128::new(1 << 24).expect("fraction denominator");
        let fraction = (denominator.get() - 1).to_f64().expect("numerator")
            / denominator.get().to_f64().expect("denominator");
        let rounded_fraction = fraction.to_f32().expect("fraction");
        assert!(rounded_fraction < 1.0);
        assert_eq!(1.0 + rounded_fraction, 2.0);

        let mut output = [0.0];
        source_frame(
            &resident,
            (2 * denominator.get() - 1, denominator),
            1.0,
            None,
            &mut output,
        )
        .expect("fractional sample below the next source frame");
        let actual = output[0];
        let expected_quadratic = (1.0 - fraction - fraction * fraction)
            .to_f32()
            .expect("quadratic value");
        assert_eq!(
            kithara_dsp::interp::quadratic([1.0, 1.0, -1.0], fraction),
            expected_quadratic,
        );
        let expected = (1.0 - 2.0 * fraction)
            .to_f32()
            .expect("convex interpolation at the exact fraction");
        assert_eq!(actual, expected);
        assert!(
            actual > -1.0,
            "the fraction must not advance to the next sample"
        );
        let phase_denominator = NonZeroU128::new(10_000_000).expect("seek phase clock");
        for taps in [
            [-1.0, 32_767.0 / 32_768.0, 32_766.0 / 32_768.0],
            [32_767.0 / 32_768.0, -1.0, -32_767.0 / 32_768.0],
            [0.0, 0.25, 0.5],
            [0.25, 0.25, 0.25],
        ] {
            let mut resident = SourceResidency::prepare(&pools, None, 0, 3, 0, 1)
                .expect("three bounded PCM frames");
            resident
                .append(
                    AudioChunkInfo {
                        spec: AudioSpec::new(1, NonZeroU32::new(44_100).expect("sample rate")),
                        frames: 3,
                        ..AudioChunkInfo::default()
                    },
                    &taps,
                )
                .expect("actual bounded PCM");
            for speed in [0.25, 1.0] {
                for (index, expected) in taps.iter().enumerate() {
                    let mut output = [0.0];
                    source_frame(
                        &resident,
                        (
                            u128::try_from(index).expect("source frame"),
                            NonZeroU128::MIN,
                        ),
                        speed,
                        None,
                        &mut output,
                    )
                    .expect("integer PCM sample");
                    let actual = output[0];
                    assert_eq!(actual.to_bits(), expected.to_bits());
                }
                for numerator in [1_u128, 2_500_000, 5_315_897, 9_999_999] {
                    let fraction = numerator.to_f64().expect("phase numerator") / 10_000_000.0;
                    let mut output = [0.0];
                    source_frame(
                        &resident,
                        (phase_denominator.get() + numerator, phase_denominator),
                        speed,
                        None,
                        &mut output,
                    )
                    .expect("fractional bounded PCM sample");
                    let actual = output[0];
                    let expected = (f64::from(taps[2]) - f64::from(taps[1]))
                        .mul_add(fraction, f64::from(taps[1]))
                        .to_f32()
                        .expect("convex PCM value at its exact phase");
                    assert_eq!(actual, expected);
                    assert!(actual.is_finite() && (-1.0..=1.0).contains(&actual));
                    assert!((taps[1].min(taps[2])..=taps[1].max(taps[2])).contains(&actual));
                }
            }
        }
    }
}
