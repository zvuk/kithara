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

#[kithara::test]
#[case(f32::NAN)]
#[case(f32::INFINITY)]
#[case(f32::NEG_INFINITY)]
fn biquad_windows_are_independent_and_refuse_nonfinite_before_mutation(#[case] nonfinite: f32) {
    let mut filter = Biquad::new(TWO, TWO).expect("filter builds");
    for section in 0..2 {
        filter
            .retune(section, coefficients(1_000.0))
            .expect("valid section");
    }
    let input = [sine_440().to_vec(), sine_440().to_vec()];
    let frames = input[0].len();
    let mut expected = input.clone();
    filter
        .process_window(&mut expected.each_mut().map(Vec::as_mut_slice), 0..frames)
        .expect("finite window");
    let mut unrelated = [vec![0.25; frames], vec![-0.5; frames]];
    filter
        .process_window(&mut unrelated.each_mut().map(Vec::as_mut_slice), 0..frames)
        .expect("finite window");
    let mut actual = input.clone();
    filter
        .process_window(&mut actual.each_mut().map(Vec::as_mut_slice), 0..frames)
        .expect("finite window");
    assert_eq!(actual, expected);
    filter.settle(&[0.25, -0.5]).expect("matching levels");
    let mut control = Biquad::new(TWO, TWO).expect("filter builds");
    for section in 0..2 {
        control
            .retune(section, coefficients(1_000.0))
            .expect("valid section");
    }
    control.copy_state(&filter).expect("matching filters");
    let mut invalid = [vec![1.0, 0.0, nonfinite], vec![0.0; 3]];
    let before = invalid
        .clone()
        .map(|plane| plane.iter().map(|x| x.to_bits()).collect::<Vec<_>>());
    assert_eq!(
        filter.process_window(&mut invalid.each_mut().map(Vec::as_mut_slice), 0..3),
        Err(FilterError::Parameters)
    );
    assert_eq!(
        invalid.map(|plane| plane.iter().map(|x| x.to_bits()).collect::<Vec<_>>()),
        before
    );
    assert_eq!(
        filter.process_window(
            &mut actual.each_mut().map(Vec::as_mut_slice),
            0..frames.saturating_add(1)
        ),
        Err(FilterError::Shape)
    );
    let mut tail = [vec![0.0; 128], vec![0.0; 128]];
    let mut twin = tail.clone();
    filter.process(&mut tail, 0..128).expect("matching planes");
    control.process(&mut twin, 0..128).expect("matching planes");
    assert_eq!(tail, twin);
}
