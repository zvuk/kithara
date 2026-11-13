#![forbid(unsafe_code)]

use std::num::NonZeroUsize;

use kithara_dsp::delay::{DelayDiffuser, DelayLine};
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
