use std::num::NonZeroUsize;

use kithara_test_fixtures::unit_fixtures::{
    accelerate_clear, accelerate_copy, accelerate_ramp, accelerate_wave,
};
use kithara_test_utils::kithara;

use super::{
    BiquadFilter, clear_f32, copy_f32, deinterleave_pair_f32, gather_f32, interleave_pair_f32,
    linear_interpolate_f32, ramp_f32, scatter_f32,
};

const THREE: NonZeroUsize = NonZeroUsize::MIN.saturating_add(2);
const SPECIALS: [f32; 8] = [
    0.0,
    -0.0,
    f32::from_bits(1),
    f32::MIN_POSITIVE,
    f32::MAX,
    f32::INFINITY,
    f32::NEG_INFINITY,
    f32::NAN,
];

fn bits<const N: usize>(values: [f32; N]) -> [u32; N] {
    values.map(f32::to_bits)
}

#[kithara::test(native, flash(false))]
fn copy_f32_matches_slice_copy(accelerate_copy: Vec<f32>) {
    let source = accelerate_copy;
    let mut target = [0.0; 4];
    assert_eq!(copy_f32(&source, &mut target), source.len());
    assert_eq!(target.as_slice(), source.as_slice());
}

#[kithara::test(native, flash(false))]
fn clear_f32_sets_zero(accelerate_clear: Vec<f32>) {
    let mut target = accelerate_clear;
    clear_f32(&mut target);
    assert_eq!(target, [0.0; 3]);
}

#[kithara::test(native, flash(false))]
fn ramp_f32_matches_scalar_ramp() {
    let mut target = [0.0; 4];
    ramp_f32(0.5, 0.25, &mut target);
    assert_eq!(target, [0.5, 0.75, 1.0, 1.25]);
}

#[kithara::test(native, flash(false))]
fn interpolation_outputs_requested_frames(accelerate_ramp: Vec<f32>) {
    let source = accelerate_ramp;
    let positions = [0.0, 0.5, 1.0];
    let mut target = [0.0; 3];
    assert_eq!(
        linear_interpolate_f32(&source, &positions, &mut target),
        target.len()
    );
}

#[kithara::test(native, flash(false))]
fn linear_interpolation_matches_scalar_positions(accelerate_ramp: Vec<f32>) {
    let source = accelerate_ramp;
    let positions = [1.0, 1.25, 1.5, 1.75];
    let mut target = [0.0; 4];

    linear_interpolate_f32(&source, &positions, &mut target);

    assert_eq!(target, [1.0, 1.25, 1.5, 1.75]);
}

#[kithara::test(native, flash(false))]
fn quadratic_interpolation_matches_scalar_positions(accelerate_wave: Vec<f32>) {
    let source = accelerate_wave;
    let positions = [1.0, 1.25, 1.5, 1.75];
    let mut target = [0.0; 4];

    super::quadratic_interpolate_f32(&source, &positions, &mut target);

    let expected = [1.0, 0.9375, 0.75, 0.4375];
    for (actual, expected) in target.iter().zip(expected) {
        assert!((actual - expected).abs() < 0.000_001);
    }
}

#[kithara::test(native, flash(false))]
fn biquad_low_pass_processes_requested_frames(accelerate_wave: Vec<f32>) {
    let Some(mut filter) =
        BiquadFilter::low_pass(44_100.0, 12_000.0, std::f64::consts::FRAC_1_SQRT_2)
    else {
        panic!("valid low pass filter");
    };
    let source = accelerate_wave;
    let mut target = [0.0; 5];
    assert_eq!(filter.process(&source, &mut target), source.len());
    assert!(target.iter().all(|sample| f32::is_finite(*sample)));
}

#[kithara::test(native, flash(false))]
fn interleave_pair_writes_only_the_common_prefix() {
    let mut output = [9.0_f32; 7];
    assert_eq!(
        interleave_pair_f32(&[1.0, 2.0, 3.0, 4.0], &[-1.0, -2.0, -3.0], &mut output),
        3
    );
    assert_eq!(bits(output), bits([1.0, -1.0, 2.0, -2.0, 3.0, -3.0, 9.0]));
}

#[kithara::test(native, flash(false))]
fn deinterleave_pair_reads_only_whole_pairs() {
    let (mut left, mut right) = ([9.0_f32; 4], [9.0_f32; 4]);
    assert_eq!(
        deinterleave_pair_f32(&[1.0, -1.0, 2.0, -2.0, 3.0], &mut left, &mut right),
        2
    );
    assert_eq!(bits(left), bits([1.0, 2.0, 9.0, 9.0]));
    assert_eq!(bits(right), bits([-1.0, -2.0, 9.0, 9.0]));
}

#[kithara::test(native, flash(false))]
fn scatter_fills_a_trailing_partial_frame() {
    let mut output = [9.0_f32; 7];
    assert_eq!(scatter_f32(&[1.0, 2.0, 3.0, 4.0], &mut output, THREE), 3);
    assert_eq!(bits(output), bits([1.0, 9.0, 9.0, 2.0, 9.0, 9.0, 3.0]));
}

#[kithara::test(native, flash(false))]
fn gather_reads_a_trailing_partial_frame() {
    let mut plane = [9.0_f32; 4];
    assert_eq!(
        gather_f32(&[1.0, 0.0, 0.0, 2.0, 0.0, 0.0, 3.0], THREE, &mut plane),
        3
    );
    assert_eq!(bits(plane), bits([1.0, 2.0, 3.0, 9.0]));
}

#[kithara::test(native, flash(false))]
fn layout_moves_special_values_bit_for_bit() {
    let mut interleaved = [1.0_f32; 16];
    assert_eq!(
        interleave_pair_f32(&SPECIALS, &SPECIALS, &mut interleaved),
        8
    );
    let (mut left, mut right) = ([1.0_f32; 8], [1.0_f32; 8]);
    assert_eq!(
        deinterleave_pair_f32(&interleaved, &mut left, &mut right),
        8
    );
    assert_eq!(bits(left), bits(SPECIALS));
    assert_eq!(bits(right), bits(SPECIALS));
}
