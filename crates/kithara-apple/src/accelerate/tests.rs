use kithara_test_fixtures::unit_fixtures::{
    accelerate_clear, accelerate_copy, accelerate_ramp, accelerate_wave,
};
use kithara_test_utils::kithara;

use super::{clear_f32, copy_f32, linear_interpolate_f32, ramp_f32};

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
