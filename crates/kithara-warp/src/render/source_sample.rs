use kithara_stretch::ElasticError;
use num_traits::ToPrimitive;

use super::renderer::residency::SourceResidency;

pub(super) fn source_sample(
    resident: &SourceResidency,
    position: (u128, std::num::NonZeroU128),
    speed: f64,
    terminal: Option<u64>,
    channels: usize,
    channel: usize,
) -> Result<f32, ElasticError> {
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
    let sample = |offset: i64| {
        let frame = source
            .checked_add(offset)
            .ok_or(ElasticError::SampleCountOverflow)?;
        let frame = last.map_or_else(|| frame.max(first), |last| frame.clamp(first, last));
        let end = u64::try_from(frame)
            .ok()
            .and_then(|frame| frame.checked_add(1))
            .ok_or(ElasticError::SampleCountOverflow)?;
        let range = resident.range(frame, end, channels)?;
        Ok::<_, ElasticError>(resident.samples[range.start + channel])
    };
    if speed <= 1.0 {
        if fraction == 0.0 {
            return sample(0);
        }
        let current = sample(0)?;
        let next = sample(1)?;
        if source == first {
            return Ok((next - current).mul_add(
                fraction.to_f32().ok_or(ElasticError::SampleCountOverflow)?,
                current,
            ));
        }
        return Ok(kithara_dsp::interp::quadratic(
            [sample(-1)?, current, next],
            fraction,
        ));
    }
    let radius = i64::try_from(crate::consts::SOURCE_RADIUS)
        .map_err(|_| ElasticError::SampleCountOverflow)?;
    let cutoff = speed.recip();
    let mut total = 0.0;
    let mut weights = 0.0;
    for offset in -radius..=radius {
        let distance = offset.to_f64().ok_or(ElasticError::SampleCountOverflow)? - fraction;
        let angle = std::f64::consts::PI * distance * cutoff;
        let sinc = if angle == 0.0 {
            cutoff
        } else {
            angle.sin() / (std::f64::consts::PI * distance)
        };
        let window = (1.0
            + (std::f64::consts::PI * distance
                / (radius + 1)
                    .to_f64()
                    .ok_or(ElasticError::SampleCountOverflow)?)
            .cos())
            * 0.5;
        let weight = sinc * window;
        total += f64::from(sample(offset)?) * weight;
        weights += weight;
    }
    (total / weights)
        .to_f32()
        .ok_or(ElasticError::SampleCountOverflow)
}

#[cfg(test)]
mod tests {
    use std::num::{NonZeroU32, NonZeroU128};

    use kithara_signal::{AudioChunkInfo, AudioSpec};
    use kithara_test_utils::kithara;

    use super::*;
    use crate::test_pools::pools;

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

        let actual = source_sample(
            &resident,
            (2 * denominator.get() - 1, denominator),
            1.0,
            None,
            1,
            0,
        )
        .expect("quadratic sample below the next source frame");
        let expected = (1.0 - fraction - fraction * fraction)
            .to_f32()
            .expect("quadratic value");
        assert_eq!(actual, expected);
        assert!(
            actual > -1.0,
            "the fraction must not advance to the next sample"
        );
    }
}
