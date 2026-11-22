#![forbid(unsafe_code)]

use kithara_dsp::filter::{FilterError, OnePoleLowPass};
use kithara_test_utils::kithara;

fn verify<const CHANNELS: usize>() {
    let input: [Vec<f32>; CHANNELS] = std::array::from_fn(|channel| {
        (0..1025)
            .map(|frame| {
                let value =
                    i16::try_from((frame + channel * 7) % 47).expect("remainder fits i16") - 23;
                f32::from(value) / 64.0
            })
            .collect()
    });
    for coefficient in [0.0_f32, 0.2, 0.875, 0.9999] {
        let mut whole =
            OnePoleLowPass::<CHANNELS>::new(coefficient).expect("fixture coefficient is admitted");
        let mut split =
            OnePoleLowPass::<CHANNELS>::new(coefficient).expect("fixture coefficient is admitted");
        let mut actual = input.clone();
        whole
            .process(&mut actual, 2..1023)
            .expect("fixture planes cover range");
        // Independent f64 state, with no production chunk/lane layout.
        for (channel, (actual, input)) in actual.iter().zip(&input).enumerate() {
            let mut state = 0.0_f64;
            for (frame, (&sample, &source)) in actual.iter().zip(input).enumerate() {
                let expected = if (2..1023).contains(&frame) {
                    state = (1.0 - f64::from(coefficient)) * f64::from(source)
                        + f64::from(coefficient) * state;
                    state
                } else {
                    f64::from(source)
                };
                assert!(
                    (f64::from(sample) - expected).abs() <= 1e-6,
                    "a {coefficient}, channel {channel}, frame {frame}: {sample} != {expected}"
                );
            }
        }
        let mut partitioned = input.clone();
        let mut start = 2;
        for count in [0, 1, 3, 257, 7].into_iter().cycle() {
            let end = (start + count).min(1023);
            split
                .process(&mut partitioned, start..end)
                .expect("fixture partition fits");
            start = end;
            if start == 1023 {
                break;
            }
        }
        for (actual, expected) in partitioned.iter().flatten().zip(actual.iter().flatten()) {
            assert_eq!(actual.to_bits(), expected.to_bits());
        }
        split.reset();
        let mut reset = input.clone();
        split
            .process(&mut reset, 2..1023)
            .expect("reset planes cover range");
        for (actual, expected) in reset.iter().flatten().zip(actual.iter().flatten()) {
            assert_eq!(actual.to_bits(), expected.to_bits());
        }
    }
}

#[kithara::test]
fn low_pass_matches_scalar_state_and_partition_reset_bits() {
    verify::<1>();
    verify::<2>();
    verify::<3>();
    verify::<4>();
    verify::<5>();
    verify::<8>();
    verify::<9>();
    let mut filter = OnePoleLowPass::<1>::new(0.5).unwrap();
    let mut impulse = vec![1.0_f32, 0.0, 0.0, 0.0, 0.0];
    filter.process(&mut [&mut impulse[..]], 0..5).unwrap();
    assert_eq!(impulse, [0.5, 0.25, 0.125, 0.0625, 0.03125]);
}

#[kithara::test]
fn low_pass_refuses_unstable_or_nonfinite_coefficients_and_zero_channels() {
    for coefficient in [
        f32::NAN,
        f32::INFINITY,
        f32::NEG_INFINITY,
        -f32::MIN_POSITIVE,
        1.0,
        1.0_f32.next_up(),
    ] {
        assert_eq!(
            OnePoleLowPass::<2>::new(coefficient).unwrap_err(),
            FilterError::Parameters
        );
    }
    assert_eq!(
        OnePoleLowPass::<0>::new(0.5).unwrap_err(),
        FilterError::Shape
    );
}

#[kithara::test]
fn low_pass_shape_refusal_keeps_all_samples_and_history() {
    let mut filter = OnePoleLowPass::<2>::new(0.75).unwrap();
    let mut control = OnePoleLowPass::<2>::new(0.75).unwrap();
    let mut warm = [vec![1.0_f32; 3], vec![-0.5_f32; 3]];
    let mut control_warm = warm.clone();
    filter.process(&mut warm, 0..3).unwrap();
    control.process(&mut control_warm, 0..3).unwrap();
    let mut malformed = [vec![0.25_f32; 16], vec![-0.25_f32; 8]];
    let original = malformed.clone();
    for range in [
        0..16,
        std::ops::Range { start: 7, end: 4 },
        usize::MAX..usize::MAX,
    ] {
        assert_eq!(
            filter.process(&mut malformed, range),
            Err(FilterError::Shape)
        );
        assert_eq!(malformed, original);
    }
    assert_eq!(
        filter.process(&mut [&mut malformed[0][..]], 0..8),
        Err(FilterError::Shape)
    );
    assert_eq!(malformed, original);
    let mut tail = [vec![0.0_f32; 129], vec![0.0_f32; 129]];
    let mut expected = tail.clone();
    filter.process(&mut tail, 0..129).unwrap();
    control.process(&mut expected, 0..129).unwrap();
    for (actual, expected) in tail.iter().flatten().zip(expected.iter().flatten()) {
        assert_eq!(actual.to_bits(), expected.to_bits());
    }
}

#[kithara::test]
fn low_pass_retune_preserves_history_and_refuses_bad_coefficients() {
    let mut filter = OnePoleLowPass::<2>::new(0.75).unwrap();
    let mut control = OnePoleLowPass::<2>::new(0.75).unwrap();
    let mut warm = [vec![1.0_f32; 13], vec![-0.5_f32; 13]];
    let mut expected_warm = warm.clone();
    filter.process(&mut warm, 0..13).unwrap();
    control.process(&mut expected_warm, 0..13).unwrap();
    for invalid in [f32::NAN, f32::INFINITY, -0.1, 1.0] {
        assert_eq!(filter.retune(invalid), Err(FilterError::Parameters));
    }
    let mut first = [[0.0_f32; 1]; 2];
    let mut unchanged = first;
    filter
        .process(&mut first.each_mut().map(<[f32; 1]>::as_mut_slice), 0..1)
        .unwrap();
    control
        .process(
            &mut unchanged.each_mut().map(<[f32; 1]>::as_mut_slice),
            0..1,
        )
        .unwrap();
    assert_eq!(
        first.map(|p| p.map(f32::to_bits)),
        unchanged.map(|p| p.map(f32::to_bits))
    );
    filter.retune(0.25).unwrap();
    let mut tail = [[0.0_f32; 17]; 2];
    filter
        .process(&mut tail.each_mut().map(<[f32; 17]>::as_mut_slice), 0..17)
        .unwrap();
    for (tail, last) in tail.iter().zip(first) {
        let mut state = f64::from(last[0]);
        for sample in tail {
            state *= 0.25;
            assert!((f64::from(*sample) - state).abs() <= 1e-6);
        }
    }
    assert!(tail[0][0] > 0.0);
}
