use kithara_test_utils::kithara;

use super::prediction_difference;

/// The phase law in `f64`: each bin predicted from the previous magnitude and
/// the previous phase advanced by the last step, a silent bin at phase zero.
/// Returns the difference and the summed magnitudes of the current and
/// previous bins, which scale its admissible error.
fn law(re: &[f32], im: &[f32], state: &mut [Vec<f64>; 3]) -> (f64, f64) {
    let [magnitudes, phases, steps] = state;
    let (mut total, mut scale) = (0.0, 0.0);
    for (((re, im), magnitude), (phase, step)) in re
        .iter()
        .zip(im)
        .zip(magnitudes.iter_mut())
        .zip(phases.iter_mut().zip(steps.iter_mut()))
    {
        let (re, im) = (f64::from(*re), f64::from(*im));
        let observed = re.hypot(im);
        let angle = if observed > 0.0 { im.atan2(re) } else { 0.0 };
        let (sin, cos) = (*phase + *step).sin_cos();
        total += (re - *magnitude * cos).hypot(im - *magnitude * sin);
        scale += observed + *magnitude;
        *step = angle - *phase;
        *phase = angle;
        *magnitude = observed;
    }
    (total, scale)
}

/// The law's state after the two frames held in `history`: the previous
/// magnitude and phase, and the step from the earlier phase to it.
fn state_of([previous_re, previous_im, earlier_re, earlier_im]: [&[f32]; 4]) -> [Vec<f64>; 3] {
    let polar = |re: &[f32], im: &[f32]| -> Vec<(f64, f64)> {
        re.iter()
            .zip(im)
            .map(|(re, im)| {
                let (x, y) = (f64::from(*re), f64::from(*im));
                let magnitude = x.hypot(y);
                (magnitude, if magnitude > 0.0 { y.atan2(x) } else { 0.0 })
            })
            .collect()
    };
    let previous = polar(previous_re, previous_im);
    let earlier = polar(earlier_re, earlier_im);
    [
        previous.iter().map(|(magnitude, _)| *magnitude).collect(),
        previous.iter().map(|(_, angle)| *angle).collect(),
        previous
            .iter()
            .zip(&earlier)
            .map(|((_, now), (_, then))| now - then)
            .collect(),
    ]
}

/// Bins around the circle at mixed radii, with silent and signed-zero bins.
fn frame(seed: usize, len: usize) -> [Vec<f32>; 2] {
    let radii = [0.0, 1.0, 0.25, 3.5, 0.0, 1.0e-3, 2.0];
    let turn = 0.37 * (f32::from(u8::try_from(seed).expect("small seed")) + 1.0);
    let mut angle = 0.0_f32;
    let (re, im) = radii
        .iter()
        .cycle()
        .skip(seed)
        .zip((0_u8..11).cycle().skip(seed))
        .take(len)
        .map(|(radius, slot)| {
            let bin = match slot {
                3 => (-0.0, 0.0),
                7 => (0.0, -0.0),
                _ => (radius * angle.cos(), radius * angle.sin()),
            };
            angle += turn;
            bin
        })
        .unzip();
    [re, im]
}

fn bits(plane: &[f32]) -> Vec<u32> {
    plane.iter().map(|value| value.to_bits()).collect()
}

fn within(got: f32, (want, scale): (f64, f64)) -> bool {
    (f64::from(got) - want).abs() <= 1e-5 * scale + f64::from(f32::MIN_POSITIVE)
}

#[kithara::test]
fn sequences_follow_the_phase_law_and_shift_the_history() {
    for len in [0, 1, 3, 4, 7, 8, 9, 17, 513, 1025] {
        let mut history: [Vec<f32>; 4] = std::array::from_fn(|_| vec![0.0; len]);
        let mut state = std::array::from_fn(|_| vec![0.0; len]);
        let mut previous = [vec![0.0_f32; len], vec![0.0; len]];
        for seed in 0..5 {
            let [re, im] = frame(seed, len);
            let want = law(&re, &im, &mut state);
            let got = prediction_difference([&re, &im], history.each_mut().map(Vec::as_mut_slice));
            assert!(
                within(got, want),
                "len {len}, frame {seed}: {got} vs {want:?}"
            );
            let [previous_re, previous_im, earlier_re, earlier_im] = &history;
            let [last_re, last_im] = &previous;
            assert_eq!(bits(previous_re), bits(&re), "len {len}, frame {seed}");
            assert_eq!(bits(previous_im), bits(&im), "len {len}, frame {seed}");
            assert_eq!(bits(earlier_re), bits(last_re), "len {len}, frame {seed}");
            assert_eq!(bits(earlier_im), bits(last_im), "len {len}, frame {seed}");
            previous = [re, im];
        }
    }
}

#[kithara::test]
fn offsets_and_short_planes_bound_the_prefix_and_keep_tails() {
    const GUARD: f32 = -7.0;
    for offset in 0_usize..9 {
        for len in [0, 1, 3, 7, 11, 17, 513] {
            let prefix = offset.saturating_add(len);
            let extent = prefix.saturating_add(2);
            for short in 0_usize..6 {
                let mut planes = frame(offset, extent);
                let [previous_re, previous_im] = frame(offset.saturating_add(1), extent);
                let [earlier_re, earlier_im] = frame(offset.saturating_add(2), extent);
                let mut history = [previous_re, previous_im, earlier_re, earlier_im];
                planes
                    .iter_mut()
                    .chain(history.iter_mut())
                    .nth(short)
                    .expect("one of six planes")
                    .truncate(prefix);
                for plane in &mut history {
                    for value in plane.iter_mut().skip(prefix) {
                        *value = GUARD;
                    }
                }
                let before = history.clone();
                let [re, im] = planes
                    .each_ref()
                    .map(|plane| plane.get(offset..).expect("input offset"));
                let mut state = state_of(
                    before
                        .each_ref()
                        .map(|plane| plane.get(offset..prefix).expect("history prefix")),
                );
                let want = law(
                    re.get(..len).expect("real prefix"),
                    im.get(..len).expect("imaginary prefix"),
                    &mut state,
                );
                let got = prediction_difference(
                    [re, im],
                    history
                        .each_mut()
                        .map(|plane| plane.get_mut(offset..).expect("history offset")),
                );
                assert!(
                    within(got, want),
                    "offset {offset}, len {len}, short {short}: {got} vs {want:?}"
                );
                for (after, before) in history.iter().zip(&before) {
                    assert_eq!(after.get(..offset), before.get(..offset));
                    assert!(after.iter().skip(prefix).all(|value| *value == GUARD));
                }
            }
        }
    }
}

#[kithara::test]
fn empty_input_returns_positive_zero() {
    let mut history = [[1.0_f32; 2]; 4];
    let got = prediction_difference([&[], &[]], history.each_mut().map(<[f32; 2]>::as_mut_slice));
    assert_eq!(got.to_bits(), 0);
    assert_eq!(history, [[1.0; 2]; 4]);
}

#[kithara::test]
fn nonfinite_bins_propagate() {
    for value in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
        let mut history: [Vec<f32>; 4] = std::array::from_fn(|_| vec![0.5; 5]);
        let mut re = vec![0.25; 5];
        *re.get_mut(2).expect("middle bin") = value;
        let got =
            prediction_difference([&re, &[0.25; 5]], history.each_mut().map(Vec::as_mut_slice));
        assert!(!got.is_finite(), "{value}: {got}");
    }
}
