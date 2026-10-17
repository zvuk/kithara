//! Checks the HLS stress suites run over one decoded saw-tooth chunk.

use std::num::{NonZeroU32, NonZeroU128};

use kithara::{platform::time::Duration, signal::SourceSpan};
use kithara_test_fixtures::signal::{self, Wave};
use num_traits::ToPrimitive;

/// The first sample outside a finite `[-1, 1]`, with its offset.
pub(crate) fn first_invalid_sample(samples: &[f32]) -> Option<(usize, f32)> {
    samples
        .iter()
        .copied()
        .enumerate()
        .find(|&(_, sample)| !(sample.is_finite() && (-1.0..=1.0).contains(&sample)))
}

/// Stereo frames whose two channels differ, as `(frame, left, right)`.
pub(crate) fn channel_mismatches(samples: &[f32]) -> impl Iterator<Item = (usize, f32, f32)> + '_ {
    samples
        .as_chunks::<2>()
        .0
        .iter()
        .enumerate()
        .filter(|(_, frame)| (frame[0] - frame[1]).abs() > f32::EPSILON)
        .map(|(f, frame)| (f, frame[0], frame[1]))
}

/// Each frame's saw phase beside the previous frame's, read from the first
/// channel, as `(frame, previous, current)`.
pub(crate) fn phase_steps(samples: &[f32], channels: usize) -> Vec<(usize, usize, usize)> {
    let phases: Vec<usize> = samples
        .chunks_exact(channels)
        .map(|frame| signal::phase::units(frame[0]))
        .collect();
    phases
        .windows(2)
        .enumerate()
        .map(|(f, pair)| (f + 1, pair[0], pair[1]))
        .collect()
}

pub(crate) fn check_fractional_saw(
    samples: &[f32],
    channels: usize,
    span: SourceSpan,
    wave: Wave,
    requested: Option<Duration>,
) -> Result<(), String> {
    if channels == 0 || samples.is_empty() || !samples.len().is_multiple_of(channels) {
        return Err("saw capture has no complete frames".to_owned());
    }
    let frames = samples.len() / channels;
    if span.output_frames() != frames as u64 {
        return Err("saw capture and source span have different frame counts".to_owned());
    }
    let first = span
        .source_ratio_at(0)
        .ok_or("saw capture has no raw origin")?;
    let origin = match requested {
        Some(position) => (
            position
                .as_nanos()
                .checked_mul(u128::from(span.sample_rate().get()))
                .ok_or("requested saw position overflows")?,
            NonZeroU128::new(1_000_000_000).expect("nanosecond clock"),
        ),
        None => first,
    };
    for frame in 0..=frames {
        let raw = span
            .source_ratio_at(frame as u64)
            .ok_or("saw frame has no raw coordinate")?;
        let expected = origin
            .0
            .checked_add(
                (frame as u128)
                    .checked_mul(origin.1.get())
                    .ok_or("saw frame overflows")?,
            )
            .ok_or("saw raw position overflows")?;
        let actual_cross = raw
            .0
            .checked_mul(origin.1.get())
            .ok_or("saw raw ratio overflows")?;
        let expected_cross = expected
            .checked_mul(raw.1.get())
            .ok_or("saw expected ratio overflows")?;
        if actual_cross != expected_cross {
            return Err(format!(
                "saw raw law differs at frame {frame}: raw={raw:?}, requested origin={origin:?}, expected numerator={expected}"
            ));
        }
        if frame == frames {
            break;
        }
        let native = usize::try_from(raw.0 / raw.1.get()).map_err(|error| error.to_string())?;
        let current = f64::from(wave.sample(native, span.sample_rate().get())) / 32_768.0;
        let fraction = (raw.0 % raw.1.get())
            .to_f64()
            .ok_or("saw fraction overflows")?
            / raw.1.get().to_f64().ok_or("saw denominator overflows")?;
        let expected = if fraction == 0.0 {
            current
        } else {
            let next = f64::from(wave.sample(
                native.checked_add(1).ok_or("next saw frame overflows")?,
                span.sample_rate().get(),
            )) / 32_768.0;
            (next - current).mul_add(fraction, current)
        }
        .to_f32()
        .ok_or("saw value is not f32")?;
        for channel in 0..channels {
            let actual = samples[frame * channels + channel];
            if !actual.is_finite() || actual != expected {
                return Err(format!(
                    "saw PCM differs at frame {frame}, channel {channel}: raw={raw:?}, expected={expected:?}, actual={actual:?}"
                ));
            }
        }
    }
    Ok(())
}

pub(crate) fn check_fractional_saw_controls() {
    let denominator = NonZeroU128::new(10_000_000).expect("seek phase clock");
    let span = SourceSpan::try_from((
        65_534 * denominator.get() + 5_315_897,
        denominator.get(),
        denominator,
        NonZeroU32::new(44_100).expect("fixture sample rate"),
        3,
    ))
    .expect("fractional saw wrap geometry");
    let expected = [
        f32::from_bits(0xbf7fff10),
        f32::from_bits(0x3d815bb2),
        f32::from_bits(0x3f7ffcf0),
    ];
    check_fractional_saw(&expected, 1, span, Wave::SawtoothDescending, None)
        .expect("exact fractional saw wrap");
    assert_eq!(
        phase_steps(&expected, 1)
            .into_iter()
            .filter(|&(_, previous, current)| signal::phase::distance(previous, current) != 1)
            .count(),
        2,
        "native integer-phase inference rejects a valid fractional wrap",
    );
    for corrupt in [
        [expected[1], expected[2], expected[2]],
        [expected[0], expected[0], expected[1]],
    ] {
        assert!(
            check_fractional_saw(&corrupt, 1, span, Wave::SawtoothDescending, None).is_err(),
            "dropped or repeated PCM must be rejected",
        );
    }
    let shifted = span
        .scaled_from(
            (0, NonZeroU128::MIN),
            (1, NonZeroU128::MIN),
            (1, NonZeroU128::MIN),
        )
        .expect("one-frame wrong phase");
    assert!(check_fractional_saw(&expected, 1, shifted, Wave::SawtoothDescending, None).is_err());
    assert!(check_fractional_saw(&expected, 1, span, Wave::Sawtooth, None).is_err());
    assert!(
        check_fractional_saw(
            &expected,
            1,
            span,
            Wave::SawtoothDescending,
            Some(Duration::ZERO),
        )
        .is_err(),
        "wrong requested origin must be rejected"
    );
}
