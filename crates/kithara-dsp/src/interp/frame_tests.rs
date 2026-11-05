use kithara_test_utils::kithara;
use num_traits::ToPrimitive;

use super::{InterpError, interpolate_frame, weighted_frames};

#[kithara::test]
fn frame_interpolation_preserves_f64_phase_and_common_prefix() {
    let current = [-0.0, -1.0, 0.75, f32::from_bits(1), f32::MAX, -0.5];
    let next = [1.0, 0.999_969_5, -0.75, -f32::from_bits(1), -f32::MAX, 0.25];
    for channels in [1, 2, 4, 6] {
        for fraction in [
            0.0,
            -0.0,
            0.25,
            0.531_589_7,
            1.0 - 2.0_f64.powi(-24),
            1.0_f64.next_down(),
        ] {
            let mut output = [7.0; 7];
            assert_eq!(
                interpolate_frame(
                    current.get(..channels).expect("fixture channel prefix"),
                    next.get(..channels).expect("fixture channel prefix"),
                    fraction,
                    &mut output
                ),
                channels
            );
            for (channel, ((&current, &next), &actual)) in current
                .iter()
                .zip(&next)
                .zip(&output)
                .take(channels)
                .enumerate()
            {
                let expected = if fraction == 0.0 {
                    current
                } else {
                    (f64::from(next) - f64::from(current))
                        .mul_add(fraction, f64::from(current))
                        .to_f32()
                        .expect("float conversion")
                };
                assert_eq!(
                    actual.to_bits(),
                    expected.to_bits(),
                    "channel {channel}, phase {fraction}"
                );
            }
            assert_eq!(
                output.get(channels..).expect("output tail"),
                [7.0; 7].get(channels..).expect("sentinel tail")
            );
        }
    }
    for (current_len, next_len, output_len) in
        [(0, 6, 7), (6, 0, 7), (6, 6, 0), (4, 2, 7), (6, 4, 1)]
    {
        let mut output = [7.0; 7];
        let count = interpolate_frame(
            current.get(..current_len).expect("fixture current prefix"),
            next.get(..next_len).expect("fixture next prefix"),
            0.25,
            output.get_mut(..output_len).expect("fixture output prefix"),
        );
        assert_eq!(count, current_len.min(next_len).min(output_len));
        assert_eq!(
            output.get(count..).expect("output tail"),
            [7.0; 7].get(count..).expect("sentinel tail")
        );
    }
    let fraction = 1.0 - 2.0_f64.powi(-24);
    let mut output = [7.0];
    interpolate_frame(&[1.0], &[-1.0], fraction, &mut output);
    let rounded = (1.0_f32 + fraction.to_f32().expect("phase")).fract();
    let mutated = (-2.0_f64)
        .mul_add(f64::from(rounded), 1.0)
        .to_f32()
        .expect("float");
    assert_ne!(
        output[0].to_bits(),
        mutated.to_bits(),
        "absolute f32 coordinates lose the phase"
    );
}

#[kithara::test]
fn frame_interpolation_preserves_special_value_semantics() {
    let current = [
        -0.0,
        f32::from_bits(0x7fc0_1234),
        f32::INFINITY,
        f32::NEG_INFINITY,
    ];
    let next = [0.0, 1.0, f32::INFINITY, 1.0];
    let mut output = [7.0; 4];
    interpolate_frame(&current, &next, 0.0, &mut output);
    assert_eq!(output.map(f32::to_bits), current.map(f32::to_bits));
    for fraction in [0.5, f64::NAN, f64::INFINITY] {
        interpolate_frame(&current, &next, fraction, &mut output);
        for ((&current, &next), &actual) in current.iter().zip(&next).zip(&output) {
            let expected = (f64::from(next) - f64::from(current))
                .mul_add(fraction, f64::from(current))
                .to_f32()
                .expect("float conversion");
            if expected.is_nan() {
                assert!(actual.is_nan());
            } else {
                assert_eq!(actual.to_bits(), expected.to_bits());
            }
        }
    }
}

#[kithara::test]
fn weighted_frames_match_ordered_scalar_channels() {
    let samples = [
        0.25,
        -0.5,
        0.75,
        -0.0,
        f32::from_bits(1),
        -1.0,
        -0.75,
        0.5,
        1.0,
        0.0,
        -f32::from_bits(1),
        0.25,
    ];
    let offsets = [6, 0, 6, 0];
    let weights = [1.0e16, 0.25, -1.0e16, 1.0_f64.next_up()];
    for channels in [1, 2, 4, 6] {
        let mut output = [7.0; 7];
        assert_eq!(
            weighted_frames(&samples, &offsets, &weights, channels, &mut output),
            Ok(())
        );
        for (channel, &actual) in output.iter().take(channels).enumerate() {
            let mut expected = 0.0;
            for (&offset, &weight) in offsets.iter().zip(&weights) {
                let index = offset.checked_add(channel).expect("fixture sample offset");
                expected += f64::from(*samples.get(index).expect("fixture sample")) * weight;
            }
            assert_eq!(actual.to_bits(), expected.to_bits());
        }
        assert_eq!(
            output.get(channels..).expect("output tail"),
            [7.0; 7].get(channels..).expect("sentinel tail")
        );
    }
    let mut output = [7.0];
    let large = 2.0_f64.powi(54);
    weighted_frames(&[1.0; 3], &[0, 1, 2], &[large, 1.0, -large], 1, &mut output)
        .expect("ordered taps");
    assert_eq!(output[0], 0.0);
    assert_ne!(output[0], (large - large) + 1.0, "tap-order mutation");
    weighted_frames(&[1.0; 3], &[0, 1, 2], &[large, -large, 1.0], 1, &mut output)
        .expect("ordered cancellation");
    assert_eq!(output[0], 1.0);
    assert_ne!(
        output[0],
        (1.0 - large) + large,
        "reversed tap-order mutation"
    );
    let value = 1.0_f32.next_up();
    let weight = 1.0_f64.next_up();
    let product = f64::from(value) * weight;
    weighted_frames(&[-1.0, value], &[0, 1], &[product, weight], 1, &mut output)
        .expect("separate multiply and add");
    assert_eq!(output[0].to_bits(), 0.0_f64.to_bits());
    assert_ne!(
        output[0].to_bits(),
        f64::from(value).mul_add(weight, -product).to_bits(),
        "FMA mutation"
    );
    for value in [f32::INFINITY, f32::NEG_INFINITY, f32::NAN] {
        weighted_frames(&[value], &[0], &[0.0], 1, &mut output)
            .expect("non-finite samples are not sanitized");
        assert!(output[0].is_nan());
    }
}

#[kithara::test]
fn weighted_shape_refusal_precedes_all_writes() {
    let samples = [0.25; 12];
    for (offsets, weights, channels, output_len) in [
        (&[][..], &[][..], 1, 7),
        (&[0][..], &[][..], 1, 7),
        (&[0][..], &[1.0, 2.0][..], 1, 7),
        (&[0][..], &[1.0][..], 6, 5),
        (&[0, 11][..], &[1.0, 1.0][..], 2, 7),
        (&[0, usize::MAX][..], &[1.0, 1.0][..], 1, 7),
        (&[0][..], &[1.0][..], usize::MAX, 7),
    ] {
        let mut output = [7.0; 7];
        assert_eq!(
            weighted_frames(
                &samples,
                offsets,
                weights,
                channels,
                output.get_mut(..output_len).expect("fixture output prefix")
            ),
            Err(InterpError::InvalidShape)
        );
        assert_eq!(output, [7.0; 7]);
    }
    let mut output = [7.0; 7];
    assert_eq!(
        weighted_frames(&[0.25], &[0], &[1.0], 2, &mut output),
        Err(InterpError::InvalidShape)
    );
    assert_eq!(output, [7.0; 7]);
    assert_eq!(
        weighted_frames(&[], &[usize::MAX], &[], 0, &mut output),
        Ok(())
    );
    assert_eq!(output, [7.0; 7]);
}
