use kithara_test_utils::kithara;
use num_traits::ToPrimitive;

use super::{Fir, bits, clean};

pub(super) fn indexed_interval<const TAPS: usize, const PHASES: usize, const HISTORY: usize>(
    taps: &[[f32; TAPS]; PHASES],
    history: &[f32; HISTORY],
    raw: &[f32],
    current: f32,
    frame: usize,
    base: isize,
) -> f32 {
    let mut peak = 0.0_f32;
    for branch in taps {
        let mut sum = 0.0_f32;
        for (tap, coefficient) in branch.iter().enumerate() {
            let position = base
                .saturating_add(tap as isize)
                .saturating_sub(HISTORY.saturating_sub(1) as isize);
            let sample = if position < frame as isize {
                usize::try_from((frame as isize).saturating_sub(position))
                    .ok()
                    .and_then(|age| HISTORY.checked_sub(age))
                    .and_then(|slot| history.get(slot).copied())
                    .unwrap_or(0.0)
            } else if position == frame as isize {
                current
            } else {
                usize::try_from(position)
                    .ok()
                    .and_then(|index| {
                        raw.get(index).copied().or_else(|| {
                            if frame.checked_add(1) == Some(raw.len()) {
                                Some(current)
                            } else {
                                raw.last().copied()
                            }
                        })
                    })
                    .unwrap_or(0.0)
            };
            sum = coefficient.mul_add(sample, sum);
        }
        peak = peak.max(sum.abs());
    }
    peak
}

fn shape<const TAPS: usize, const PHASES: usize, const HISTORY: usize>() {
    let taps: [[f32; TAPS]; PHASES] = std::array::from_fn(|phase: usize| {
        std::array::from_fn(|tap: usize| {
            let position = phase.checked_add(tap).unwrap();
            let sign = if position.is_multiple_of(2) {
                1.0
            } else {
                -1.0
            };
            sign * position.checked_add(1).unwrap().to_f32().unwrap() * 0.03125
        })
    });
    let mut actual = Fir::<TAPS, PHASES, 2, HISTORY>::new(taps);
    let mut reference = actual.clone();
    for (stage, frames) in [33_usize, 1, 0, 7, 2, 13, 1, 0, 4].into_iter().enumerate() {
        let mut raw: [Vec<f32>; 2] = std::array::from_fn(|channel| {
            (0..frames.checked_add(channel.checked_mul(2).unwrap()).unwrap())
                .map(|frame| {
                    let value = *[0.0, -0.0, 0.125, -0.25, 2.0, -2.0]
                        .get(frame.checked_add(channel).unwrap() % 6)
                        .unwrap();
                    if stage % 3 == 0 && frame == frames / 2 {
                        *[f32::NAN, f32::INFINITY].get(channel).unwrap()
                    } else if stage % 3 == 1 {
                        value * 0.001
                    } else {
                        value
                    }
                })
                .collect()
        });
        actual.begin_block(&raw.each_mut().map(Vec::as_mut_slice), 0.98);
        reference.begin_block(&raw.each_mut().map(Vec::as_mut_slice), 0.98);
        for (channel, raw) in raw.iter().enumerate() {
            let current: Vec<_> = raw.iter().take(frames).copied().map(clean).collect();
            let mut expected = vec![f32::MIN; frames.checked_add(2).unwrap()];
            for (frame, &sample) in current.iter().enumerate() {
                let mut peak = sample.abs().max(0.0);
                if reference.reconstruct {
                    let history = reference.history.get(channel).unwrap();
                    let ahead =
                        indexed_interval(&taps, history, raw, sample, frame, frame as isize);
                    let behind = if *reference.shared_valid.get(channel).unwrap() {
                        *reference.shared_peak.get(channel).unwrap()
                    } else {
                        indexed_interval(
                            &taps,
                            history,
                            raw,
                            sample,
                            frame,
                            (frame as isize).checked_sub(1).unwrap(),
                        )
                    };
                    *reference.shared_peak.get_mut(channel).unwrap() = ahead;
                    peak = peak.max(0.0_f32.max(ahead).max(behind));
                }
                *expected.get_mut(frame).unwrap() = peak;
                let history = reference.history.get_mut(channel).unwrap();
                if HISTORY > 0 {
                    history.copy_within(1.., 0);
                    *history.last_mut().unwrap() = sample;
                }
                *reference.shared_valid.get_mut(channel).unwrap() = true;
            }
            let mut output = vec![f32::MIN; frames.checked_add(2).unwrap()];
            for start in (0..frames).step_by(3) {
                let end = frames.min(start.checked_add(3).unwrap());
                assert_eq!(
                    actual.process(
                        channel,
                        raw.get(start..).unwrap(),
                        current.get(start..end).unwrap(),
                        output.get_mut(start..end).unwrap(),
                    ),
                    end.checked_sub(start).unwrap()
                );
            }
            assert_eq!(bits(output), bits(expected));
        }
        assert_eq!(
            bits(actual.history.into_iter().flatten()),
            bits(reference.history.into_iter().flatten())
        );
        assert_eq!(bits(actual.shared_peak), bits(reference.shared_peak));
        assert_eq!(actual.shared_valid, reference.shared_valid);
        assert_eq!(actual.reconstruct, reference.reconstruct);
        if stage == 4 {
            actual.reset();
            reference.reset();
        }
    }
}

#[kithara::test]
fn generic_regions_keep_exact_peaks_and_state_across_varying_blocks() {
    shape::<0, 2, 0>();
    shape::<1, 0, 3>();
    shape::<1, 2, 0>();
    shape::<2, 2, 0>();
    shape::<3, 2, 1>();
    shape::<2, 2, 4>();
    shape::<9, 2, 3>();
    shape::<17, 7, 9>();
    shape::<17, 17, 9>();

    let mut no_channels = Fir::<3, 2, 0, 0>::new([[1.0; 3]; 2]);
    let mut raw = [2.0];
    no_channels.begin_block(&[&mut raw], 0.98);
    let mut untouched = [-0.0, f32::MIN];
    assert_eq!(no_channels.process(0, &raw, &raw, &mut untouched), 0);
    assert_eq!(bits(untouched), bits([-0.0, f32::MIN]));
}

#[kithara::test]
fn zero_padding_still_participates_in_the_fused_recurrence() {
    let mut detector = Fir::<3, 1, 1, 0>::new([[f32::INFINITY, 1.0, 1.0]]);
    let mut raw = [f32::NAN, 0.5];
    detector.begin_block(&[&mut raw], 0.98);
    let mut peaks = [f32::MIN; 2];
    assert_eq!(detector.process(0, &raw, &[0.0], &mut peaks[..1]), 1);
    assert_eq!(bits(peaks), bits([0.0, f32::MIN]));
    assert_ne!(
        peaks.first().unwrap().to_bits(),
        1.0_f32.mul_add(0.5, 0.0).to_bits()
    );
}
