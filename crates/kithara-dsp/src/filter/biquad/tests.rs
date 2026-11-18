use std::num::NonZeroUsize;

use kithara_test_fixtures::portable::sine_440;
use kithara_test_utils::kithara;

use super::super::{Biquad, Coefficients, FilterError, Hertz, Type};

const TWO: NonZeroUsize = NonZeroUsize::MIN.saturating_add(1);

fn coefficients(cutoff: f64) -> Coefficients<f64> {
    Coefficients::from_params(
        Type::LowPass,
        Hertz::from_hz(48_000.0).expect("positive rate"),
        Hertz::from_hz(cutoff).expect("positive cutoff"),
        std::f64::consts::FRAC_1_SQRT_2,
    )
    .expect("stable design")
}

#[kithara::test]
fn biquad_broadcast_equals_retuning_every_plane() {
    let mut broadcast = Biquad::new(TWO, TWO).expect("filter builds");
    let mut per_plane = Biquad::new(TWO, TWO).expect("filter builds");
    for cutoff in [1_000.0, 4_000.0] {
        for section in 0..2 {
            broadcast
                .retune(section, coefficients(cutoff))
                .expect("section exists");
            for plane in 0..2 {
                per_plane
                    .retune_plane(plane, section, coefficients(cutoff))
                    .expect("plane exists");
            }
        }
        let mut actual = [sine_440().to_vec(), sine_440().to_vec()];
        let mut expected = actual.clone();
        let frames = actual[0].len();
        broadcast
            .process(&mut actual, 0..frames)
            .expect("matching shape");
        per_plane
            .process(&mut expected, 0..frames)
            .expect("matching shape");
        for (actual, expected) in actual.iter().flatten().zip(expected.iter().flatten()) {
            assert_eq!(actual.to_bits(), expected.to_bits());
        }
    }
}

#[kithara::test]
fn biquad_refused_plane_retunes_preserve_design_and_history() {
    let mut filter = Biquad::new(TWO, TWO).expect("filter builds");
    let mut control = Biquad::new(TWO, TWO).expect("filter builds");
    for section in 0..2 {
        for (plane, cutoff) in [100.0, 5_000.0].into_iter().enumerate() {
            filter
                .retune_plane(plane, section, coefficients(cutoff))
                .expect("plane exists");
            control
                .retune_plane(plane, section, coefficients(cutoff))
                .expect("plane exists");
        }
    }
    filter.settle(&[0.25, -0.5]).expect("matching levels");
    control.settle(&[0.25, -0.5]).expect("matching levels");
    let mut invalid = coefficients(1_000.0);
    invalid.b0 = f64::NAN;
    for (plane, section, design, error) in [
        (2, 0, coefficients(1_000.0), FilterError::Shape),
        (0, 2, coefficients(1_000.0), FilterError::Shape),
        (0, 0, invalid, FilterError::Parameters),
    ] {
        assert_eq!(filter.retune_plane(plane, section, design), Err(error));
    }
    let mut actual = [vec![0.25; 128], vec![-0.5; 128]];
    let mut expected = actual.clone();
    filter
        .process(&mut actual, 0..128)
        .expect("matching planes");
    control
        .process(&mut expected, 0..128)
        .expect("matching planes");
    for (plane, level) in actual.iter().zip([0.25_f32, -0.5]) {
        assert!(plane.iter().all(|sample| (*sample - level).abs() < 0.000_1));
    }
    for (actual, expected) in actual.iter().flatten().zip(expected.iter().flatten()) {
        assert_eq!(actual.to_bits(), expected.to_bits());
    }
}
