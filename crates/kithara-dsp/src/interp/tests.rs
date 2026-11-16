use std::num::NonZeroU32;

use kithara_test_utils::kithara;

use super::{InterpError, Interpolation, RateRamp, interpolate, sinc_hann_weights};

const METHODS: [Interpolation; 4] = [
    Interpolation::Linear,
    Interpolation::Quadratic,
    Interpolation::Hermite,
    Interpolation::Watte,
];
const UNWRITTEN: f32 = -1.0;
/// One frame past the largest window an `f32` position addresses exactly.
const OVERSIZED: usize = 16_777_217;
const SMALL: [f32; 6] = [0.0, 1.0, 2.0, 3.0, 4.0, 5.0];
/// No `−0.0` and no zero: at `x = 0` the formulas return `y0` bit for bit
/// only for other values.
const WINDOW: [f32; 8] = [0.1, -0.7, 0.3, 0.9, -0.2, 0.5, -0.4, 0.8];

fn bits(values: &[f32]) -> Vec<u32> {
    values.iter().map(|value| value.to_bits()).collect()
}

#[kithara::test]
fn padding_names_the_frames_each_method_reads() {
    assert_eq!(
        METHODS.map(Interpolation::padding),
        [(0, 1), (1, 1), (1, 2), (1, 2)]
    );
}

#[kithara::test]
fn out_of_window_positions_fail_without_writing() {
    let oversized = vec![0.0_f32; OVERSIZED];
    let len = u8::try_from(SMALL.len()).expect("six frames");
    for method in METHODS {
        let (before, after) = method.padding();
        let low = f32::from(before);
        let cases: [(&[f32], f32); 4] = [
            (SMALL.as_slice(), f32::NAN),
            (SMALL.as_slice(), f32::from(len.saturating_sub(after))),
            (SMALL.as_slice(), low - 0.5),
            (oversized.as_slice(), 1.5),
        ];
        for (window, bad) in cases {
            let mut output = [UNWRITTEN; 2];
            assert_eq!(
                interpolate(method, window, &[low, bad], &mut output),
                Err(InterpError::OutOfWindow),
                "{method:?} at {bad} in {} frames",
                window.len()
            );
            assert_eq!(bits(&output), bits(&[UNWRITTEN; 2]), "{method:?} at {bad}");
        }
    }
}

#[kithara::test]
fn integer_positions_return_the_window_samples() {
    let len = u8::try_from(WINDOW.len()).expect("eight frames");
    for method in METHODS {
        let (before, after) = method.padding();
        let positions: Vec<f32> = (before..len.saturating_sub(after)).map(f32::from).collect();
        let mut output = vec![UNWRITTEN; positions.len()];
        assert_eq!(
            interpolate(method, &WINDOW, &positions, &mut output),
            Ok(positions.len()),
            "{method:?}"
        );
        let expected: Vec<f32> = WINDOW
            .iter()
            .copied()
            .skip(usize::from(before))
            .take(positions.len())
            .collect();
        assert_eq!(bits(&output), bits(&expected), "{method:?}");
    }
}

/// A 24-frame ramp from 1.0 to 1.5.
const RAMP_FRAMES: NonZeroU32 = NonZeroU32::MIN.saturating_add(23);
const STEP: f64 = 0.5 / 24.0;
const OFFSET_TOLERANCE: f64 = 1.0e-12;

fn ramp() -> RateRamp {
    RateRamp::new(1.0, 1.5, RAMP_FRAMES)
}

#[kithara::test]
fn offsets_are_the_running_sum_of_the_rates() {
    let ramp = ramp();
    let mut sum = 0.0_f64;
    for (frame, step) in (0..40_usize).zip(0_u32..) {
        let offset = ramp.offset(frame);
        assert!(
            (offset - sum).abs() < OFFSET_TOLERANCE,
            "frame {frame}: {offset} against {sum}"
        );
        sum += if step < 24 {
            f64::from(step).mul_add(STEP, 1.0)
        } else {
            1.5
        };
    }
}

#[kithara::test]
fn a_ramp_split_across_blocks_advances_like_one_block() {
    let ramp = ramp();
    let split = ramp.offset(10) + ramp.after(10).offset(20);
    let whole = ramp.offset(30);
    assert!(
        (split - whole).abs() < OFFSET_TOLERANCE,
        "{split} against {whole}"
    );
}

#[kithara::test]
fn a_ramp_lands_on_its_target_and_holds_it() {
    let ramp = ramp();
    assert!(ramp.after(23).held().is_none());
    for frames in [24, 1_000] {
        assert_eq!(
            ramp.after(frames).held().map(f64::to_bits),
            Some(1.5_f64.to_bits()),
            "after {frames} frames"
        );
    }
    assert_eq!(ramp.after(24).current().to_bits(), 1.5_f64.to_bits());
}

#[kithara::test]
fn peak_is_the_fastest_rate_of_the_block() {
    let up = ramp();
    let down = RateRamp::new(1.5, 1.0, RAMP_FRAMES);
    assert_eq!(up.peak(0).to_bits(), 1.0_f64.to_bits());
    assert!((up.peak(10) - 9.0_f64.mul_add(STEP, 1.0)).abs() < OFFSET_TOLERANCE);
    assert_eq!(up.peak(40).to_bits(), 1.5_f64.to_bits());
    assert_eq!(down.peak(10).to_bits(), 1.5_f64.to_bits());
}

#[kithara::test]
fn positions_stop_before_the_end() {
    let hold = RateRamp::hold(1.25);
    let mut output = [UNWRITTEN; 8];
    assert_eq!(hold.positions(1.0, 6.0, &mut output), 4);
    assert_eq!(
        bits(&output),
        bits(&[
            1.0, 2.25, 3.5, 4.75, UNWRITTEN, UNWRITTEN, UNWRITTEN, UNWRITTEN
        ])
    );
    assert_eq!(hold.offset(4).to_bits(), 5.0_f64.to_bits());
    assert_eq!(hold.positions(f64::NAN, 6.0, &mut output), 0);
}

/// The Hann-windowed sinc evaluated directly, one tap at a time.
///
/// `window_radius` is the positive distance from the window's center to its zero.
/// Callers supply finite distances inside that radius and a cutoff in `(0, 1]`.
fn sinc_hann_weight(distance: f64, cutoff: f64, window_radius: f64) -> f64 {
    let angle = std::f64::consts::PI * distance * cutoff;
    let sinc = if angle == 0.0 {
        cutoff
    } else {
        angle.sin() / (std::f64::consts::PI * distance)
    };
    let window = (1.0 + (std::f64::consts::PI * distance / window_radius).cos()) * 0.5;
    sinc * window
}

#[kithara::test]
#[case::whole_tap(0.0, 1.0)]
#[case::fractional_unity_cutoff(0.375, 1.0)]
#[case::downsampling(0.5, 1.0 / 1.088_435)]
#[case::steep_downsampling(0.999, 0.25)]
fn rotated_taps_match_the_scalar_weight(#[case] fraction: f64, #[case] cutoff: f64) {
    const RADIUS: i32 = 16;
    let width = f64::from(RADIUS + 1);
    let mut weights = [0.0; 2 * RADIUS as usize + 1];
    sinc_hann_weights(-f64::from(RADIUS) - fraction, cutoff, width, &mut weights);
    for (tap, weight) in (-RADIUS..=RADIUS).zip(weights) {
        let expected = sinc_hann_weight(f64::from(tap) - fraction, cutoff, width);
        assert!(
            (weight - expected).abs() <= 1e-12,
            "tap {tap}: {weight} vs {expected}"
        );
    }
}
