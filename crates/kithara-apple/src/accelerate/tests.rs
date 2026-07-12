use super::{BiquadFilter, clear_f32, copy_f32, linear_interpolate_f32, ramp_f32};

#[test]
fn copy_f32_matches_slice_copy() {
    let source = [1.0, -2.0, 3.5, 4.25];
    let mut target = [0.0; 4];
    assert_eq!(copy_f32(&source, &mut target), source.len());
    assert_eq!(target, source);
}

#[test]
fn clear_f32_sets_zero() {
    let mut target = [1.0, -2.0, 3.0];
    clear_f32(&mut target);
    assert_eq!(target, [0.0; 3]);
}

#[test]
fn ramp_f32_matches_scalar_ramp() {
    let mut target = [0.0; 4];
    ramp_f32(0.5, 0.25, &mut target);
    assert_eq!(target, [0.5, 0.75, 1.0, 1.25]);
}

#[test]
fn interpolation_outputs_requested_frames() {
    let source = [0.0, 1.0, 2.0, 3.0];
    let positions = [0.0, 0.5, 1.0];
    let mut target = [0.0; 3];
    assert_eq!(
        linear_interpolate_f32(&source, &positions, &mut target),
        target.len()
    );
}

#[test]
fn linear_interpolation_matches_scalar_positions() {
    let source = [0.0, 1.0, 2.0, 3.0];
    let positions = [1.0, 1.25, 1.5, 1.75];
    let mut target = [0.0; 4];

    linear_interpolate_f32(&source, &positions, &mut target);

    assert_eq!(target, [1.0, 1.25, 1.5, 1.75]);
}

#[test]
fn quadratic_interpolation_matches_scalar_positions() {
    let source = [0.0, 1.0, 0.0, -1.0, 0.0];
    let positions = [1.0, 1.25, 1.5, 1.75];
    let mut target = [0.0; 4];

    super::quadratic_interpolate_f32(&source, &positions, &mut target);

    let expected = [1.0, 0.9375, 0.75, 0.4375];
    for (actual, expected) in target.iter().zip(expected) {
        assert!((actual - expected).abs() < 0.000_001);
    }
}

#[test]
fn biquad_low_pass_processes_requested_frames() {
    let Some(mut filter) =
        BiquadFilter::low_pass(44_100.0, 12_000.0, std::f64::consts::FRAC_1_SQRT_2)
    else {
        panic!("valid low pass filter");
    };
    let source = [0.0, 1.0, 0.0, -1.0, 0.0];
    let mut target = [0.0; 5];
    assert_eq!(filter.process(&source, &mut target), source.len());
    assert!(target.iter().all(|sample| f32::is_finite(*sample)));
}
