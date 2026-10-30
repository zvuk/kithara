use std::f32::consts::PI;

use fearless_simd::{Level, dispatch};
use kithara_test_utils::kithara;
use num_traits::ToPrimitive;

use super::Fir;

type Detector = Fir<17, 7, 8, 9>;

fn coefficients() -> [[f32; 17]; 7] {
    std::array::from_fn(|phase| {
        std::array::from_fn(|tap| {
            let distance = phase.checked_add(1).unwrap().to_f32().unwrap() / 8.0
                - (tap.to_f32().unwrap() - 8.0);
            if distance.abs() < f32::EPSILON {
                1.0
            } else {
                let argument = PI * distance;
                argument.sin() / argument * (0.5 * (1.0 + (PI * distance / 8.0).cos()))
            }
        })
    })
}

pub(super) fn clean(value: f32) -> f32 {
    if value.is_normal() { value } else { 0.0 }
}

fn interval(
    taps: &[[f32; 17]; 7],
    history: &[f32; 9],
    block: &[f32],
    frame: usize,
    base: isize,
) -> f32 {
    let mut peak = 0.0_f32;
    for branch in taps {
        let mut sum = 0.0_f32;
        for (tap, coefficient) in branch.iter().enumerate() {
            let position = base
                .checked_add(tap as isize)
                .unwrap()
                .checked_sub(8)
                .unwrap();
            let sample = if position < frame as isize {
                let age = (frame as isize).checked_sub(position).unwrap();
                usize::try_from(9_isize.checked_sub(age).unwrap())
                    .ok()
                    .and_then(|slot| history.get(slot).copied())
                    .unwrap_or(0.0)
            } else {
                usize::try_from(position)
                    .ok()
                    .and_then(|index| block.get(index).or_else(|| block.last()).copied())
                    .unwrap_or(0.0)
            };
            sum = coefficient.mul_add(sample, sum);
        }
        peak = peak.max(sum.abs());
    }
    peak
}

pub(super) fn bits(values: impl IntoIterator<Item = f32>) -> Vec<u32> {
    values.into_iter().map(f32::to_bits).collect()
}

fn assert_state(actual: &Detector, expected: &Detector) {
    assert_eq!(
        bits(actual.taps.into_iter().flatten()),
        bits(expected.taps.into_iter().flatten())
    );
    assert_eq!(
        bits(actual.history.into_iter().flatten()),
        bits(expected.history.into_iter().flatten())
    );
    assert_eq!(bits(actual.shared_peak), bits(expected.shared_peak));
    assert_eq!(actual.shared_valid, expected.shared_valid);
    assert_eq!(actual.reconstruct, expected.reconstruct);
}

#[kithara::test]
fn ordered_intervals_and_complete_state_match_the_scalar_base() {
    let taps = coefficients();
    for channels in [1_usize, 2, 8] {
        for frames in [0_usize, 1, 7, 255, 256, 257, 600] {
            let mut detector = Detector::new(taps);
            let mut fallback = detector.clone();
            let mut history = [[0.0_f32; 9]; 8];
            let mut shared = [0.0_f32; 8];
            for scale in [0.01, 2.0, 0.98, 0.001] {
                let mut raw: Vec<Vec<f32>> = (0..channels)
                    .map(|channel| {
                        (0..frames.checked_add(channel).unwrap())
                            .map(|frame| {
                                let value = *[0.0, -0.0, 0.02, -0.03, 0.4, -0.1, 0.98]
                                    .get(frame % 7)
                                    .unwrap();
                                if scale == 0.98 && frame == frames / 2 {
                                    *[f32::NAN, f32::INFINITY, f32::NEG_INFINITY]
                                        .get(channel % 3)
                                        .unwrap()
                                } else {
                                    value * scale
                                }
                            })
                            .collect()
                    })
                    .collect();
                let mut expected = raw.clone();
                detector.begin_block(
                    &raw.iter_mut().map(Vec::as_mut_slice).collect::<Vec<_>>(),
                    0.98,
                );
                dispatch!(Level::fallback(), simd => fallback.begin_block_kernel(
                    simd,
                    &raw.iter_mut().map(Vec::as_mut_slice).collect::<Vec<_>>(),
                    0.98,
                ));
                let maximum = expected
                    .iter()
                    .flatten()
                    .chain(history.iter().take(channels).flatten())
                    .fold(0.0_f32, |peak, &sample| {
                        peak.max(if sample.is_finite() {
                            sample.abs()
                        } else {
                            f32::INFINITY
                        })
                    });
                let reconstruct = frames > 0
                    && (maximum > 0.98
                        || taps.iter().any(|branch| {
                            branch.iter().fold(0.0_f32, |sum, coefficient| {
                                coefficient.abs().mul_add(maximum, sum)
                            }) > 0.98
                        }));
                assert_eq!(detector.reconstruct, reconstruct);
                for channel in 0..channels {
                    let expected_channel = expected.get_mut(channel).unwrap();
                    let channel_history = history.get_mut(channel).unwrap();
                    let channel_shared = shared.get_mut(channel).unwrap();
                    let raw_channel = raw.get(channel).unwrap();
                    let mut wanted = vec![0.0_f32; frames];
                    for frame in 0..frames {
                        let sample = expected_channel.get_mut(frame).unwrap();
                        *sample = clean(*sample);
                        let sample = *sample;
                        let mut peak = sample.abs();
                        if reconstruct {
                            let ahead = interval(
                                &taps,
                                channel_history,
                                expected_channel,
                                frame,
                                frame as isize,
                            );
                            let behind = if frame == 0 {
                                interval(
                                    &taps,
                                    channel_history,
                                    expected_channel,
                                    frame,
                                    (frame as isize).checked_sub(1).unwrap(),
                                )
                            } else {
                                *channel_shared
                            };
                            *channel_shared = ahead;
                            peak = peak.max(ahead.max(behind));
                        }
                        *wanted.get_mut(frame).unwrap() = peak;
                        channel_history.copy_within(1.., 0);
                        *channel_history.get_mut(8).unwrap() = sample;
                    }
                    let mut actual = vec![f32::MIN; frames.checked_add(2).unwrap()];
                    let mut fallback_peaks = actual.clone();
                    let current: Vec<f32> = raw_channel
                        .iter()
                        .take(frames)
                        .copied()
                        .map(clean)
                        .collect();
                    for start in (0..frames).step_by(256) {
                        let end = frames.min(start.checked_add(256).unwrap());
                        let raw = raw_channel.get(start..).unwrap();
                        let samples = current.get(start..end).unwrap();
                        assert_eq!(
                            detector.process(
                                channel,
                                raw,
                                samples,
                                actual.get_mut(start..end).unwrap()
                            ),
                            end.checked_sub(start).unwrap()
                        );
                        let output = fallback_peaks.get_mut(start..end).unwrap();
                        let processed = if fallback.reconstruct {
                            dispatch!(Level::fallback(), simd => fallback.process_kernel(simd, channel, raw, samples, output))
                        } else {
                            fallback.process(channel, raw, samples, output)
                        };
                        assert_eq!(processed, end.checked_sub(start).unwrap());
                    }
                    assert_eq!(bits(fallback_peaks), bits(actual.iter().copied()));
                    assert_eq!(
                        bits(actual.get(..frames).unwrap().iter().copied()),
                        bits(wanted)
                    );
                    assert_eq!(actual.get(frames..).unwrap(), &[f32::MIN; 2]);
                }
                assert_eq!(
                    bits(detector.history.into_iter().flatten()),
                    bits(history.into_iter().flatten())
                );
                assert_eq!(bits(detector.shared_peak), bits(shared));
                for (channel, valid) in detector.shared_valid.iter().enumerate() {
                    assert_eq!(*valid, channel < channels && frames > 0);
                }
                assert_state(&detector, &fallback);
            }
            detector.reset();
            assert_state(&detector, &Detector::new(taps));
        }
    }
}

#[kithara::test]
fn behind_interval_and_external_boundary_are_load_bearing() {
    let taps = coefficients();
    let history = [2.0, -2.0, 1.9, -1.9, 1.8, -1.8, 1.7, -1.7, 0.1];
    let mut detector = Detector::new(taps);
    detector.history[0] = history;
    let mut raw = [0.1, 0.2, 0.3, 0.4, -2.0, 2.0, -1.0];
    detector.begin_block(&[&mut raw], 0.98);
    let ahead = interval(&taps, &history, &raw, 0, 0);
    let behind = interval(&taps, &history, &raw, 0, -1);
    assert!(
        behind > ahead,
        "the behind-interval negative control must limit"
    );
    let mut peaks = [0.0];
    detector.process(0, &raw, &[raw[0]], &mut peaks);
    assert_eq!(peaks[0].to_bits(), behind.to_bits());
    assert_ne!(peaks[0].to_bits(), ahead.to_bits());

    let mut complete = Detector::new(taps);
    let mut truncated = complete.clone();
    complete.begin_block(&[&mut raw], 0.98);
    truncated.begin_block(&[&mut raw], 0.98);
    let mut correct = [0.0; 3];
    let mut wrong = correct;
    complete.process(0, &raw, &raw[..3], &mut correct);
    truncated.process(0, &raw[..3], &raw[..3], &mut wrong);
    assert_ne!(
        bits(correct),
        bits(wrong),
        "holding an internal chunk tail must be detected"
    );
}
