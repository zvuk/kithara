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
    let sample_frame = |offset: i64| {
        let frame = source
            .checked_add(offset)
            .ok_or(ElasticError::SampleCountOverflow)?;
        Ok::<_, ElasticError>(
            last.map_or_else(|| frame.max(first), |last| frame.clamp(first, last)),
        )
    };
    if speed <= 1.0 {
        let sample_offset = |offset| {
            let frame = sample_frame(offset)?;
            let end = u64::try_from(frame)
                .ok()
                .and_then(|frame| frame.checked_add(1))
                .ok_or(ElasticError::SampleCountOverflow)?;
            Ok::<_, ElasticError>(resident.range(frame, end, channels)?.start)
        };
        return interpolate_source_frame(resident, sample_offset, fraction, output);
    }
    downsample_source_frame(resident, source, sample_frame, fraction, speed, output)
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

/// Admits the first missing tap before a later coordinate overflow.
fn resident_tap_window(
    resident: &SourceResidency,
    first: i64,
    last: i64,
    channels: usize,
) -> Result<std::ops::Range<usize>, ElasticError> {
    let available = resident
        .samples
        .len()
        .saturating_sub(resident.offset)
        .checked_div(channels);
    let resident_end = match available {
        Some(frames) => Some(
            i128::from(resident.start)
                + i128::try_from(frames).map_err(|_| ElasticError::SampleCountOverflow)?,
        ),
        None => None,
    };
    let end = i128::from(last) + 1;
    let end = match resident_end {
        Some(resident_end) => end.min(resident_end + 1),
        None if resident.offset <= resident.samples.len() => end,
        None => i128::from(first) + 1,
    }
    .max(i128::from(first) + 1);
    let end = u64::try_from(end).map_err(|_| ElasticError::SampleCountOverflow)?;
    resident.range(first, end, channels)
}

fn downsample_source_frame(
    resident: &SourceResidency,
    source: i64,
    sample_frame: impl Fn(i64) -> Result<i64, ElasticError>,
    fraction: f64,
    speed: f64,
    output: &mut [f32],
) -> Result<(), ElasticError> {
    let radius = i64::try_from(crate::consts::SOURCE_RADIUS)
        .map_err(|_| ElasticError::SampleCountOverflow)?;
    let cutoff = speed.recip();
    let channels = output.len();
    let first = sample_frame(-radius)?;
    let last = sample_frame(radius.min(i64::MAX - source))?;
    let window = resident_tap_window(resident, first, last, channels)?;
    source
        .checked_add(radius)
        .ok_or(ElasticError::SampleCountOverflow)?;
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
        let frame = sample_frame(offset)?;
        let relative =
            usize::try_from(frame - first).map_err(|_| ElasticError::SampleCountOverflow)?;
        *tap = (window.start + relative * channels, weight);
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
        const MONO: [f32; 64] = {
            let mut samples = [0.0; 64];
            let mut frame = 0;
            let mut value = 0.0;
            while frame < samples.len() {
                samples[frame] = value;
                value += 1.0 / 64.0;
                frame += 1;
            }
            samples
        };
        const STEREO: [f32; 128] = {
            let mut samples = [0.0; 128];
            let mut frame = 0;
            while frame < MONO.len() {
                samples[frame * 2] = MONO[frame];
                samples[frame * 2 + 1] = -MONO[frame];
                frame += 1;
            }
            samples
        };
        const QUAD: [f32; 256] = {
            let mut samples = [0.0; 256];
            let mut frame = 0;
            while frame < MONO.len() {
                samples[frame * 4] = MONO[frame];
                samples[frame * 4 + 1] = -MONO[frame];
                samples[frame * 4 + 2] = 0.5;
                samples[frame * 4 + 3] = -0.75;
                frame += 1;
            }
            samples
        };

        let pools = pools();
        let mut resident =
            SourceResidency::prepare(&pools, None, 32, 64, 0, 1).expect("retained source history");
        resident
            .append(
                AudioChunkInfo {
                    spec: AudioSpec::new(1, NonZeroU32::new(44_100).expect("sample rate")),
                    frames: 64,
                    ..AudioChunkInfo::default()
                },
                &MONO,
            )
            .expect("exact mapped source span");
        let mut output = [0.0];
        source_frame(&resident, (16, NonZeroU128::MIN), 4.0, None, &mut output)
            .expect("all filter taps are in the resident source window");
        let actual = output[0];
        assert_eq!(actual, 0.25);
        for (channel_count, samples) in [(2, &STEREO[..]), (4, &QUAD[..])] {
            let channels = usize::from(channel_count);
            let mut resident = SourceResidency::prepare(&pools, None, 32, 64, 0, channels)
                .expect("independent channel history");
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
                    samples,
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

    #[kithara::test]
    fn resident_tap_window_matches_scalar_pcm_and_errors() {
        let maximum = u64::try_from(i64::MAX).expect("signed source limit");
        let phase = NonZeroU128::new(3).expect("fractional source clock");
        for channels in [0_usize, 1, 2, 4] {
            let source_channels = channels.max(1);
            let spec = AudioSpec::new(
                u16::try_from(source_channels).expect("test channels"),
                NonZeroU32::new(44_100).expect("sample rate"),
            );
            let samples: Vec<_> = (0..64 * source_channels)
                .map(|index| {
                    f32::from(i16::try_from(index % 257).expect("bounded sample")) / 256.0 - 0.5
                })
                .collect();
            for (start, origin, frames, retained) in [
                (0, Some(0), 64, 0),
                (17, Some(17), 64, 0),
                (64, Some(0), 64, 3),
                (maximum - 16, Some(0), 1, 0),
                (maximum - 32, Some(0), 32, 0),
                (maximum, Some(maximum), 1, 0),
                (0, None, 0, 0),
            ] {
                let pools = pools();
                let mut resident =
                    SourceResidency::prepare(&pools, None, 0, 64, 0, source_channels)
                        .expect("resident fixture");
                resident
                    .append(
                        AudioChunkInfo {
                            spec,
                            frame_offset: start,
                            frames,
                            ..AudioChunkInfo::default()
                        },
                        &samples
                            [..usize::try_from(frames).expect("fixture frames") * source_channels],
                    )
                    .expect("contiguous prepared source");
                resident.origin = origin;
                resident.start += i64::try_from(retained).expect("retained frames");
                resident.offset = retained * source_channels;
                let first = origin.unwrap_or(0);
                for source in [
                    0,
                    start,
                    start.saturating_add(16).min(maximum),
                    start.saturating_add(32).min(maximum),
                    maximum,
                ] {
                    for fraction in 0_u128..3 {
                        let position = (u128::from(source) * 3 + fraction, phase);
                        for speed in [0.25, 1.0, 1.0_f64.next_up(), 4.0] {
                            for terminal in [
                                None,
                                Some(start + u64::from(frames)),
                                Some(first),
                                Some(first + 1),
                            ] {
                                if terminal.is_some_and(|end| end.saturating_sub(1) < first) {
                                    continue;
                                }
                                let mut actual = vec![0.125; channels];
                                let mut expected = actual.clone();
                                let reference = scalar_source_frame(
                                    &resident,
                                    position,
                                    speed,
                                    terminal,
                                    &mut expected,
                                );
                                let result =
                                    source_frame(&resident, position, speed, terminal, &mut actual);
                                assert_eq!(result, reference);
                                for (actual, expected) in actual.into_iter().zip(expected) {
                                    assert_eq!(actual.to_bits(), expected.to_bits());
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    #[kithara::test]
    fn resident_tap_window_preserves_partial_storage_and_zero_channel_admission() {
        for (offset, truncate, channels, source) in [
            (129, 0, 2, 1_000_000_u64),
            (129, 0, 0, 1_000_000),
            (0, 1, 2, 81),
            (0, 0, 0, 1_000_000),
            (0, 0, 0, u64::from(u32::MAX) + 32),
        ] {
            let pools = pools();
            let mut resident = SourceResidency::prepare(&pools, None, 0, 64, 0, 2)
                .expect("resident stereo source");
            resident
                .append(
                    AudioChunkInfo {
                        spec: AudioSpec::new(2, NonZeroU32::new(44_100).expect("sample rate")),
                        frame_offset: 17,
                        frames: 64,
                        ..AudioChunkInfo::default()
                    },
                    &[0.25; 128],
                )
                .expect("resident stereo frames");
            resident.offset = offset;
            resident.samples.truncate(128 - truncate);
            let mut actual = vec![0.125; channels];
            let mut expected = actual.clone();
            let position = (u128::from(source), NonZeroU128::MIN);
            let reference = scalar_source_frame(&resident, position, 4.0, None, &mut expected);
            let result = source_frame(&resident, position, 4.0, None, &mut actual);
            assert_eq!(result, reference);
            assert_eq!(
                actual
                    .iter()
                    .map(|sample| sample.to_bits())
                    .collect::<Vec<_>>(),
                expected
                    .iter()
                    .map(|sample| sample.to_bits())
                    .collect::<Vec<_>>()
            );
            if offset != 0 || truncate != 0 {
                assert_eq!(
                    result,
                    Err(ElasticError::EnginePreparation(
                        "required source is not resident"
                    ))
                );
                assert!(actual.iter().all(|sample| *sample == 0.125));
            } else if source == 1_000_000 {
                assert_eq!(result, Ok(()));
            }
        }
    }

    #[kithara::test]
    fn first_missing_tap_precedes_later_coordinate_overflow() {
        let maximum = u64::try_from(i64::MAX).expect("signed source limit");
        let pools = pools();
        let mut resident =
            SourceResidency::prepare(&pools, None, 0, 1, 0, 1).expect("one resident source frame");
        resident
            .append(
                AudioChunkInfo {
                    spec: AudioSpec::new(1, NonZeroU32::new(44_100).expect("sample rate")),
                    frame_offset: maximum - 16,
                    frames: 1,
                    ..AudioChunkInfo::default()
                },
                &[0.25],
            )
            .expect("source immediately before the signed limit");
        let mut output = [0.125];
        assert_eq!(
            source_frame(
                &resident,
                (u128::from(maximum), NonZeroU128::MIN),
                4.0,
                None,
                &mut output,
            ),
            Err(ElasticError::EnginePreparation(
                "required source is not resident"
            )),
        );
        assert_eq!(output, [0.125]);
    }

    fn scalar_source_frame(
        resident: &SourceResidency,
        position: (u128, NonZeroU128),
        speed: f64,
        terminal: Option<u64>,
        output: &mut [f32],
    ) -> Result<(), ElasticError> {
        let channels = output.len();
        let (numerator, denominator) = position;
        let denominator = denominator.get();
        let source = i64::try_from(numerator / denominator)
            .map_err(|_| ElasticError::SampleCountOverflow)?;
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
        scalar_downsample_source_frame(resident, sample_offset, fraction, speed, output)
    }

    fn scalar_downsample_source_frame(
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
}
