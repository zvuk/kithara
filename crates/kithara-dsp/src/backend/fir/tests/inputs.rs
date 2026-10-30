use kithara_test_utils::kithara;

use super::{Fir, bits, shapes::indexed_interval};

fn indexed_process<const TAPS: usize, const HISTORY: usize>(
    detector: &mut Fir<TAPS, 2, 1, HISTORY>,
    raw: &[f32],
    current: &[f32],
    output: &mut [f32],
) -> usize {
    let count = raw.len().min(current.len()).min(output.len());
    let history = detector.history.first_mut().unwrap();
    let shared = detector.shared_peak.first_mut().unwrap();
    let valid = detector.shared_valid.first_mut().unwrap();
    for (frame, (&sample, peak)) in current.iter().zip(output).take(count).enumerate() {
        *peak = sample.abs().max(0.0);
        if detector.reconstruct {
            let ahead =
                indexed_interval(&detector.taps, history, raw, sample, frame, frame as isize);
            let behind = if *valid {
                *shared
            } else {
                indexed_interval(
                    &detector.taps,
                    history,
                    raw,
                    sample,
                    frame,
                    (frame as isize).checked_sub(1).unwrap(),
                )
            };
            *shared = ahead;
            *peak = peak.max(0.0_f32.max(ahead).max(behind));
        }
        if HISTORY > 0 {
            history.copy_within(1.., 0);
            *history.last_mut().unwrap() = sample;
        }
        *valid = true;
    }
    count
}

fn assert_ieee(actual: &[f32], expected: &[f32]) {
    assert_eq!(actual.len(), expected.len());
    for (&actual, &expected) in actual.iter().zip(expected) {
        if expected.is_nan() {
            assert!(actual.is_nan());
        } else {
            assert_eq!(actual.to_bits(), expected.to_bits());
        }
    }
}

fn assert_state<const TAPS: usize, const HISTORY: usize>(
    actual: &Fir<TAPS, 2, 1, HISTORY>,
    expected: &Fir<TAPS, 2, 1, HISTORY>,
) {
    assert_eq!(
        bits(actual.taps.into_iter().flatten()),
        bits(expected.taps.into_iter().flatten())
    );
    assert_eq!(
        bits(actual.history.into_iter().flatten()),
        bits(expected.history.into_iter().flatten())
    );
    assert_ieee(&actual.shared_peak, &expected.shared_peak);
    assert_eq!(actual.shared_valid, expected.shared_valid);
    assert_eq!(actual.reconstruct, expected.reconstruct);
}

fn uncleaned_current<const HISTORY: usize>() {
    let taps = [[0.25; 17], [-0.125; 17]];
    let mut actual = Fir::<17, 2, 1, HISTORY>::new(taps);
    let mut expected = actual.clone();
    let mut history = [0.0; HISTORY];
    let values = [
        -0.0,
        f32::from_bits(1),
        -f32::from_bits(1),
        f32::MIN_POSITIVE,
        f32::from_bits(0x7fc0_1234),
        f32::INFINITY,
        f32::NEG_INFINITY,
        0.0,
        0.125,
        -0.25,
        -0.0,
    ];
    for stage in 0..3 {
        let current: [f32; 11] = std::array::from_fn(|frame| {
            *values
                .get(
                    frame
                        .checked_add(stage)
                        .unwrap()
                        .checked_rem(values.len())
                        .unwrap(),
                )
                .unwrap()
        });
        let mut raw: [f32; 14] =
            std::array::from_fn(|frame| *[2.0, -3.0, 4.0, -5.0, 6.0].get(frame % 5).unwrap());
        actual.begin_block(&[&mut raw], 0.98);
        expected.begin_block(&[&mut raw], 0.98);
        assert_eq!(bits(*actual.history.first().unwrap()), bits(history));
        assert_state(&actual, &expected);
        let mut start = 0_usize;
        for count in [3_usize, 1, 7] {
            let end = start.checked_add(count).unwrap();
            let input = current.get(start..end).unwrap();
            let raw = raw.get(start..).unwrap();
            let mut output = [f32::MIN; 9];
            let mut wanted = output;
            assert_eq!(actual.process(0, raw, input, &mut output), count);
            assert_eq!(
                indexed_process(&mut expected, raw, input, &mut wanted),
                count
            );
            assert_ieee(&output, &wanted);
            assert_state(&actual, &expected);
            for &sample in input {
                if HISTORY > 0 {
                    history.copy_within(1.., 0);
                    *history.last_mut().unwrap() = sample;
                }
            }
            assert_eq!(bits(*actual.history.first().unwrap()), bits(history));
            start = end;
        }
    }
}

#[kithara::test]
fn current_and_history_preserve_uncleaned_ieee_inputs() {
    uncleaned_current::<0>();
    uncleaned_current::<1>();
    uncleaned_current::<9>();
}

fn prefixes<const HISTORY: usize>(threshold: f32) {
    let taps = [[0.25; 17], [-0.125; 17]];
    let mut actual = Fir::<17, 2, 1, HISTORY>::new(taps);
    let mut expected = actual.clone();
    let mut raw = [2.0, -3.0, 4.0, -5.0, 6.0, -7.0];
    let current = [-0.0, 0.25, -0.5, 0.125];
    actual.begin_block(&[&mut raw], threshold);
    expected.begin_block(&[&mut raw], threshold);
    assert_eq!(actual.reconstruct, threshold == 0.98);
    let mut output = [0.0; 4];
    let mut wanted = output;
    assert_eq!(actual.process(0, &raw, &current, &mut output), 4);
    assert_eq!(
        indexed_process(&mut expected, &raw, &current, &mut wanted),
        4
    );
    assert_ieee(&output, &wanted);
    assert_state(&actual, &expected);
    for boundary in [false, true] {
        if boundary {
            actual.begin_block(&[&mut raw], threshold);
            expected.begin_block(&[&mut raw], threshold);
        }
        for (raw_len, current_len, output_len) in [(0, 4, 6), (6, 0, 6), (6, 4, 0)] {
            let before = actual.clone();
            let mut output = [-0.0, f32::MIN, 0.25, -0.5, f32::INFINITY, f32::from_bits(1)];
            let sentinel = output;
            assert_eq!(
                actual.process(
                    0,
                    raw.get(..raw_len).unwrap(),
                    current.get(..current_len).unwrap(),
                    output.get_mut(..output_len).unwrap(),
                ),
                0
            );
            assert_eq!(bits(output), bits(sentinel));
            assert_state(&actual, &before);
        }
    }
    for (raw_len, current_len, output_len) in [(6, 4, 2), (2, 4, 6), (6, 1, 6)] {
        let mut output = [-0.0, f32::MIN, 0.25, -0.5, f32::INFINITY, f32::from_bits(1)];
        let mut wanted = output;
        let raw = raw.get(..raw_len).unwrap();
        let current = current.get(..current_len).unwrap();
        let count = raw_len.min(current_len).min(output_len);
        assert_eq!(
            actual.process(0, raw, current, output.get_mut(..output_len).unwrap()),
            count
        );
        assert_eq!(
            indexed_process(
                &mut expected,
                raw,
                current,
                wanted.get_mut(..output_len).unwrap()
            ),
            count
        );
        assert_ieee(&output, &wanted);
        assert_state(&actual, &expected);
    }
}

#[kithara::test]
fn warmed_empty_and_mismatched_prefixes_keep_state_and_sentinels() {
    for threshold in [0.98, 32.0] {
        prefixes::<0>(threshold);
        prefixes::<1>(threshold);
        prefixes::<9>(threshold);
    }
}

fn zero_windows<const HISTORY: usize>() {
    for coefficient in [0.25, f32::INFINITY, f32::NAN] {
        let mut actual = Fir::<17, 2, 1, HISTORY>::new([[coefficient; 17], [-0.125; 17]]);
        let mut expected = actual.clone();
        for (stage, frames) in [9_usize, 1, 7, 128, 1024, 0, 7, 13].into_iter().enumerate() {
            let mut raw = vec![if stage == 0 { 2.0 } else { 0.0 }; frames.checked_add(3).unwrap()];
            let mut current = vec![if stage == 0 { 2.0 } else { -0.0 }; frames];
            if stage == 7 {
                for (index, sample) in current.iter_mut().enumerate() {
                    *sample = *[
                        f32::NAN,
                        f32::INFINITY,
                        f32::NEG_INFINITY,
                        -0.0,
                        f32::from_bits(1),
                    ]
                    .get(index % 5)
                    .unwrap();
                }
            }
            let threshold = if stage % 2 == 0 { -1.0 } else { 0.98 };
            actual.begin_block(&[&mut raw], threshold);
            expected.begin_block(&[&mut raw], threshold);
            if stage == 7 {
                assert!(!actual.reconstruct);
            }
            let mut output = vec![f32::MIN; frames.checked_add(1).unwrap()];
            let mut wanted = output.clone();
            for start in (0..frames).step_by(7) {
                let end = frames.min(start.checked_add(7).unwrap());
                let raw = raw.get(start..).unwrap();
                let current = current.get(start..end).unwrap();
                assert_eq!(
                    actual.process(0, raw, current, output.get_mut(start..end).unwrap()),
                    indexed_process(
                        &mut expected,
                        raw,
                        current,
                        wanted.get_mut(start..end).unwrap()
                    )
                );
            }
            assert_ieee(&output, &wanted);
            assert_state(&actual, &expected);
            if stage == 4 {
                actual.reset();
                expected.reset();
            }
        }
    }
}

#[kithara::test]
fn transient_zero_windows_preserve_complete_state_and_coefficient_specials() {
    zero_windows::<0>();
    zero_windows::<1>();
    zero_windows::<9>();
}
