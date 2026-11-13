#![forbid(unsafe_code)]

use std::num::NonZeroUsize;

use kithara_dsp::delay::{DelayDiffuser, DelayError, DelayLine, DiffuserError};
use kithara_test_utils::{bufpool::pools, kithara};

const TWO: NonZeroUsize = NonZeroUsize::MIN.saturating_add(1);

fn diffuser(delay: usize, feedback: f32) -> DelayDiffuser<2> {
    let line = DelayLine::new(
        TWO,
        NonZeroUsize::new(delay).expect("positive delay"),
        &pools(),
    )
    .expect("small history fits");
    DelayDiffuser::new(line, feedback).expect("stable coefficient")
}

// Freeverb-form scalar oracle: w[n] = x[n] + g*w[n-D], y[n] = w[n-D] - x[n].
// Unbounded f64 history is independent of the production ring and span layout.
fn scalar(input: &[f32], delay: usize, feedback: f32) -> Vec<f64> {
    let mut history = Vec::with_capacity(input.len());
    input
        .iter()
        .enumerate()
        .map(|(frame, &input)| {
            let delayed = frame
                .checked_sub(delay)
                .and_then(|past| history.get(past))
                .copied()
                .unwrap_or(0.0);
            history.push(f64::from(input) + f64::from(feedback) * delayed);
            delayed - f64::from(input)
        })
        .collect()
}

#[kithara::test]
fn diffuser_matches_scalar_impulse_wrap_reset_and_independent_channels() {
    for delay in [1, 3, 17, 257] {
        for feedback in [0.0, 0.5, -0.625, 0.875] {
            for sustained in [false, true] {
                let mut input = [vec![0.0; 2309], vec![0.0; 2309]];
                input[0][0] = 1.0;
                if sustained {
                    let [left, right] = &mut input;
                    for (frame, (left, right)) in left.iter_mut().zip(right).enumerate().skip(7) {
                        *left = f32::from(u16::try_from(frame % 31).unwrap()) / 128.0;
                        *right = -f32::from(u16::try_from(frame % 19).unwrap()) / 64.0;
                    }
                }
                let expected = input.each_ref().map(|plane| scalar(plane, delay, feedback));
                let mut section = diffuser(delay, feedback);
                let mut actual = input.clone();
                section.process(&mut actual, 0..2309).expect("valid planes");
                for (channel, (actual, expected)) in actual.iter().zip(expected).enumerate() {
                    for (frame, (&actual, expected)) in actual.iter().zip(expected).enumerate() {
                        assert!(
                            (f64::from(actual) - expected).abs() <= 1e-6,
                            "delay {delay}, g {feedback}, channel {channel}, frame {frame}: \
                             {actual} != {expected}"
                        );
                    }
                }
                section.reset();
                let mut after_reset = input;
                section
                    .process(&mut after_reset, 0..2309)
                    .expect("valid planes");
                for (before, after) in actual.iter().flatten().zip(after_reset.iter().flatten()) {
                    assert_eq!(before.to_bits(), after.to_bits());
                }
            }
        }
    }

    let mut impulse = [
        vec![1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
        vec![0.0; 10],
    ];
    diffuser(3, 0.5).process(&mut impulse, 0..10).unwrap();
    assert_eq!(
        impulse[0],
        [-1.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.5, 0.0, 0.0, 0.25]
    );
    assert!(impulse[1].iter().all(|sample| *sample == 0.0));
}

#[kithara::test]
fn diffuser_is_bit_identical_across_uneven_partitions() {
    let input = [
        (0..4110)
            .map(|frame| f32::from(u16::try_from(frame % 97).unwrap()) / 256.0 - 0.125)
            .collect::<Vec<_>>(),
        (0..4110)
            .map(|frame| -f32::from(u16::try_from(frame % 37).unwrap()) / 128.0)
            .collect::<Vec<_>>(),
    ];
    for delay in [1, 3, 127, 128, 129, 257] {
        let mut whole = diffuser(delay, 0.731);
        let mut split = diffuser(delay, 0.731);
        let mut expected = input.clone();
        let mut actual = input.clone();
        whole.process(&mut expected, 2..4107).unwrap();
        let mut start = 2;
        for span in [0, 1, 19, 127, 513, 2, 256, 11].into_iter().cycle() {
            let end = (start + span).min(4107);
            split.process(&mut actual, start..end).unwrap();
            start = end;
            if start == 4107 {
                break;
            }
        }
        for (actual, expected) in actual.iter().flatten().zip(expected.iter().flatten()) {
            assert_eq!(actual.to_bits(), expected.to_bits(), "delay {delay}");
        }
        for (actual, input) in actual.iter().zip(&input) {
            for range in [0..2, 4107..4110] {
                for (actual, input) in actual[range.clone()].iter().zip(&input[range]) {
                    assert_eq!(actual.to_bits(), input.to_bits());
                }
            }
        }
        let mut whole_tail = [vec![0.0; 1021], vec![0.0; 1021]];
        let mut split_tail = whole_tail.clone();
        whole.process(&mut whole_tail, 0..1021).unwrap();
        split.process(&mut split_tail, 0..1021).unwrap();
        for (actual, expected) in split_tail.iter().flatten().zip(whole_tail.iter().flatten()) {
            assert_eq!(actual.to_bits(), expected.to_bits(), "tail, delay {delay}");
        }
    }
}

#[kithara::test]
fn diffuser_refuses_bad_shapes_without_changing_samples_or_history() {
    let mut section = diffuser(3, 0.5);
    let mut control = diffuser(3, 0.5);
    let mut warm = [vec![1.0; 7], vec![-0.5; 7]];
    let mut control_warm = warm.clone();
    section.process(&mut warm, 0..7).unwrap();
    control.process(&mut control_warm, 0..7).unwrap();

    let mut malformed = [vec![0.125; 16], vec![-0.25; 8]];
    let original = malformed.clone();
    for range in [
        0..16,
        std::ops::Range { start: 7, end: 4 },
        usize::MAX..usize::MAX,
    ] {
        assert_eq!(
            section.process(&mut malformed, range),
            Err(DiffuserError::Delay(DelayError::Shape))
        );
        assert_eq!(malformed, original);
    }
    assert_eq!(
        section.process(&mut [&mut malformed[0][..]], 0..8),
        Err(DiffuserError::Delay(DelayError::Shape))
    );
    assert_eq!(malformed, original);

    let mut after = [vec![0.0; 127], vec![0.0; 127]];
    let mut expected = after.clone();
    section.process(&mut after, 0..127).unwrap();
    control.process(&mut expected, 0..127).unwrap();
    for (actual, expected) in after.iter().flatten().zip(expected.iter().flatten()) {
        assert_eq!(actual.to_bits(), expected.to_bits());
    }
}

#[kithara::test]
fn diffuser_refuses_unstable_feedback_and_mismatched_history() {
    for feedback in [
        f32::NAN,
        f32::INFINITY,
        f32::NEG_INFINITY,
        -1.0,
        1.0,
        -1.01,
        1.01,
    ] {
        let line = DelayLine::new(TWO, NonZeroUsize::MIN, &pools()).unwrap();
        assert_eq!(
            DelayDiffuser::<2>::new(line, feedback).unwrap_err(),
            DiffuserError::Feedback
        );
    }
    let line = DelayLine::new(TWO, NonZeroUsize::MIN, &pools()).unwrap();
    assert_eq!(
        DelayDiffuser::<0>::new(line, 0.5).unwrap_err(),
        DiffuserError::Delay(DelayError::Shape)
    );
}
