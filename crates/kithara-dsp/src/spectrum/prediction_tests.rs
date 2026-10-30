use std::f32::consts::{PI, TAU};

use kithara_test_utils::kithara;

use super::prediction_difference;

// Bounds checks between tuple uses can change the original paired sin_cos lowering.
fn scalar(input: [&[f32]; 4], state: &mut [Vec<f32>; 3]) -> f32 {
    let [re, im, observed, angle] = input;
    let [magnitudes, phases, steps] = state;
    let bins = re.iter().zip(im).zip(observed.iter().zip(angle));
    let history = magnitudes
        .iter_mut()
        .zip(phases.iter_mut())
        .zip(steps.iter_mut());
    let mut total = 0.0_f32;
    for (((re, im), (observed, angle)), ((magnitude, phase), step)) in bins.zip(history) {
        let previous = *phase;
        let predicted = previous + *step;
        let (sin, cos) = (predicted - TAU * (predicted / TAU).round()).sin_cos();
        total += (re - *magnitude * cos).hypot(im - *magnitude * sin);
        let delta = angle - previous;
        *step = delta - TAU * (delta / TAU).round();
        *phase = *angle;
        *magnitude = *observed;
    }
    total
}

fn same(actual: f32, expected: f32) {
    if expected.is_nan() {
        assert!(actual.is_nan());
    } else {
        assert_eq!(actual.to_bits(), expected.to_bits());
    }
}

#[kithara::test]
fn prediction_matches_scalar_state_for_offsets_and_every_short_plane() {
    let values = [
        0.0,
        -0.0,
        1.0,
        -2.0,
        PI,
        -PI,
        TAU,
        -TAU,
        0.25,
        1.0e20,
        f32::from_bits(1),
    ];
    for offset in 0_usize..16 {
        for len in [0, 1, 3, 7, 11, 17, 513] {
            for short in 0_usize..7 {
                let prefix = offset.saturating_add(len);
                let extent = prefix.saturating_add(2);
                let mut inputs: [Vec<f32>; 4] = std::array::from_fn(|plane| {
                    values
                        .iter()
                        .copied()
                        .cycle()
                        .skip(plane)
                        .take(extent)
                        .collect()
                });
                let mut state: [Vec<f32>; 3] = std::array::from_fn(|plane| {
                    values
                        .iter()
                        .copied()
                        .cycle()
                        .skip(plane.saturating_add(4))
                        .take(extent)
                        .collect()
                });
                if short < 4 {
                    inputs.get_mut(short).expect("input plane").truncate(prefix);
                } else {
                    state
                        .get_mut(short.saturating_sub(4))
                        .expect("state plane")
                        .truncate(prefix);
                }
                let mut expected = state.clone();
                for _ in 0..3 {
                    let input = inputs
                        .each_ref()
                        .map(|s| s.get(offset..).expect("fixture offset"));
                    let mut tail = expected
                        .each_ref()
                        .map(|s| s.get(offset..).expect("state offset").to_vec());
                    let want = scalar(input, &mut tail);
                    for (plane, tail) in expected.iter_mut().zip(tail) {
                        plane
                            .get_mut(offset..)
                            .expect("state offset")
                            .copy_from_slice(&tail);
                    }
                    let [re, im, observed, angle] = input;
                    let [magnitude, phase, step] = &mut state;
                    let got = prediction_difference(
                        [re, im],
                        [observed, angle],
                        [
                            magnitude.get_mut(offset..).expect("magnitude offset"),
                            phase.get_mut(offset..).expect("phase offset"),
                            step.get_mut(offset..).expect("step offset"),
                        ],
                    );
                    same(got, want);
                    for (actual, expected) in state.iter().zip(&expected) {
                        for (&actual, &expected) in actual.iter().zip(expected) {
                            same(actual, expected);
                        }
                    }
                }
            }
        }
    }
}

#[kithara::test]
fn half_turn_ties_and_old_phase_determine_the_next_step() {
    let mut magnitude = [0.0; 3];
    let mut phase = [0.0, 0.0, PI / 2.0];
    let mut step = [0.0; 3];
    prediction_difference(
        [&[1.0; 3], &[0.0; 3]],
        [&[1.0; 3], &[PI, -PI, -PI / 2.0]],
        [&mut magnitude, &mut phase, &mut step],
    );
    assert_eq!(step.map(f32::to_bits), [-PI, PI, PI].map(f32::to_bits));
    assert_eq!(
        phase.map(f32::to_bits),
        [PI, -PI, -PI / 2.0].map(f32::to_bits)
    );
    assert_eq!(magnitude, [1.0; 3]);
}

#[kithara::test]
fn special_values_follow_scalar_transcendentals_without_sanitizing() {
    let values = [
        f32::NAN,
        f32::INFINITY,
        f32::NEG_INFINITY,
        f32::MAX,
        -0.0,
        f32::from_bits(1),
    ];
    for plane in 0_usize..7 {
        for value in values {
            let mut input = [[0.25; 3]; 4];
            let mut state = [vec![0.5; 3], vec![0.0; 3], vec![0.0; 3]];
            if plane < 4 {
                *input
                    .get_mut(plane)
                    .expect("input plane")
                    .get_mut(1)
                    .expect("middle bin") = value;
            } else {
                *state
                    .get_mut(plane.saturating_sub(4))
                    .expect("state plane")
                    .get_mut(1)
                    .expect("middle bin") = value;
            }
            let mut expected = state.clone();
            let want = scalar(input.each_ref().map(<[f32; 3]>::as_slice), &mut expected);
            let [re, im, observed, angle] = &input;
            let [magnitude, phase, step] = &mut state;
            let got = prediction_difference([re, im], [observed, angle], [magnitude, phase, step]);
            same(got, want);
            for (actual, expected) in state.iter().flatten().zip(expected.iter().flatten()) {
                same(*actual, *expected);
            }
        }
    }
}
