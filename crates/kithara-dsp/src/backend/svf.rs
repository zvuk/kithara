use std::{
    f64::consts::PI,
    ops::{Add, Div, Mul, Sub},
};

use fearless_simd::{Level, dispatch, f64x2, prelude::*};
use num_traits::ToPrimitive;

use super::platform;
use crate::{
    consts,
    filter::{Response, SVF_CHUNK_FRAMES},
};

#[inline(always)]
pub(crate) fn step<S: Simd>(
    simd: S,
    x: f64x2<S>,
    [s1, s2]: [f64x2<S>; 2],
    coefficients: [f64; 3],
    damping: f64,
    response: Response,
) -> ([f64x2<S>; 2], f64x2<S>) {
    let [a1, a2, a3] = coefficients.map(|value| f64x2::splat(simd, value));
    let damping = f64x2::splat(simd, damping);
    let two = f64x2::splat(simd, 2.0);
    let v3 = x.sub(s2);
    let v1 = a1.mul(s1).add(a2.mul(v3));
    let v2 = s2.add(a2.mul(s1)).add(a3.mul(v3));
    let state = [two.mul(v1).sub(s1), two.mul(v2).sub(s2)];
    let output = match response {
        Response::LowPass => v2,
        Response::HighPass => x.sub(damping.mul(v1)).sub(v2),
        Response::AllPass => x.sub(two.mul(damping).mul(v1)),
    };
    (state, output)
}

#[inline(always)]
pub(super) fn cutoff_angles<'a, S: Simd>(
    simd: S,
    cutoffs: &[f64],
    rate: f64,
    angles: &'a mut [f64],
) -> &'a mut [f64] {
    let len = cutoffs.len().min(angles.len());
    let Some((angles, _)) = angles.split_at_mut_checked(len) else {
        return &mut [];
    };
    let pi = f64x2::splat(simd, PI);
    let rate = f64x2::splat(simd, rate);
    for (cutoffs, angles) in cutoffs.chunks(2).zip(angles.chunks_mut(2)) {
        let cutoff = f64x2::from_fn(simd, |lane| cutoffs.get(lane).copied().unwrap_or(0.0));
        let values = pi.mul(cutoff).div(rate);
        for (angle, value) in angles.iter_mut().zip(values.as_slice()).take(cutoffs.len()) {
            *angle = *value;
        }
    }
    angles
}

#[cfg(test)]
mod tests {
    use fearless_simd::{Level, dispatch};
    use kithara_test_utils::kithara;

    use super::*;
    use crate::backend::{platform, portable};

    fn cutoff_angles<S: Simd>(simd: S, cutoffs: &[f64], rate: f64, angles: &mut [f64]) -> usize {
        super::cutoff_angles(simd, cutoffs, rate, angles).len()
    }

    #[kithara::test]
    fn cutoff_angles_and_portable_tangents_keep_scalar_order_and_prefix() {
        for level in [Level::fallback(), Level::new()] {
            for len in [0, 1, 2, 3, 13, 255, 256, 257] {
                let cutoffs: Vec<_> = [f64::from_bits(1), 1e-305, 20.0, 1_000.0, 23_999.0]
                    .into_iter()
                    .cycle()
                    .take(len)
                    .collect();
                let mut output = vec![-7.0; len.saturating_add(1)];
                dispatch!(level, simd => {
                    assert_eq!(cutoff_angles(simd, &cutoffs, 48_000.0, &mut output), len);
                });
                for (actual, cutoff) in output.iter().zip(&cutoffs) {
                    assert_eq!(actual.to_bits(), (PI * cutoff / 48_000.0).to_bits());
                }
                dispatch!(level, simd => portable::svf_gains(simd, &cutoffs, 48_000.0, &mut output));
                for (actual, cutoff) in output.iter().zip(&cutoffs) {
                    assert_eq!(actual.to_bits(), (PI * cutoff / 48_000.0).tan().to_bits());
                }
                assert_eq!(
                    output.get(len).expect("unwritten tail").to_bits(),
                    (-7.0_f64).to_bits()
                );
            }
        }
    }

    #[kithara::test]
    fn platform_gains_preserve_double_precision_and_the_unwritten_tail() {
        let cutoffs = [
            f64::from_bits(1),
            1e-305,
            20.0,
            400.0,
            20_000.0,
            23_999.0,
            f64::from_bits(
                24_000.0_f64
                    .to_bits()
                    .checked_sub(1)
                    .expect("positive float bits"),
            ),
        ];
        let mut gains = [-7.0; 8];
        dispatch!(Level::new(), simd => platform::svf_gains(simd, &cutoffs, 48_000.0, &mut gains));
        for (actual, cutoff) in gains.iter().zip(cutoffs) {
            let expected = (PI * cutoff / 48_000.0).tan();
            if expected.abs() < f64::MIN_POSITIVE {
                assert_eq!(actual.to_bits(), expected.to_bits());
            } else {
                assert!((actual - expected).abs() <= 8.0 * f64::EPSILON * expected.abs());
            }
        }
        assert_eq!(gains[7], -7.0);
    }
}

pub(crate) use crate::filter::StateVariableFilter as State;

impl<const CHANNELS: usize> State<CHANNELS> {
    #[inline(always)]
    pub(super) fn frame_samples<S: Simd>(
        &mut self,
        simd: S,
        samples: &mut [Option<&mut f32>],
        gain: f64,
        damping: f64,
        response: Response,
    ) {
        if self.reset_state
            && samples
                .iter()
                .all(|sample| sample.as_deref().is_none_or(|sample| sample.to_bits() == 0))
        {
            self.count(true);
            return;
        }
        let coefficients = coefficients(gain, damping);
        let mut frame_quiet = true;
        let [memory1, memory2] = &mut self.state;
        for ((samples, memory1), memory2) in samples
            .chunks_mut(2)
            .zip(memory1.chunks_mut(2))
            .zip(memory2.chunks_mut(2))
        {
            let s1 = f64x2::from_fn(simd, |lane| memory1.get(lane).copied().unwrap_or(0.0));
            let s2 = f64x2::from_fn(simd, |lane| memory2.get(lane).copied().unwrap_or(0.0));
            let x = f64x2::from_fn(simd, |lane| {
                samples
                    .get(lane)
                    .and_then(Option::as_deref)
                    .copied()
                    .map_or(0.0, f64::from)
            });
            let ([next1, next2], output) = step(simd, x, [s1, s2], coefficients, damping, response);
            for (memory, value) in memory1.iter_mut().zip(next1.as_slice()) {
                *memory = *value;
            }
            for (memory, value) in memory2.iter_mut().zip(next2.as_slice()) {
                *memory = *value;
            }
            for (sample, output) in samples.iter_mut().zip(output.as_slice()) {
                if let Some(sample) = sample.as_deref_mut() {
                    let output = output.to_f32().unwrap_or(f32::NAN);
                    frame_quiet &=
                        sample.abs() <= consts::SILENT_PEAK && output.abs() <= consts::SILENT_PEAK;
                    *sample = output;
                }
            }
        }
        self.reset_state = false;
        self.count(frame_quiet);
    }

    fn count(&mut self, frame_quiet: bool) {
        self.quiet = if frame_quiet {
            self.quiet.saturating_add(1).min(consts::SECTION_MEMORY)
        } else {
            0
        };
        if self.quiet == consts::SECTION_MEMORY {
            self.clear();
        }
    }
}

pub(crate) fn process<const CHANNELS: usize>(
    state: &mut State<CHANNELS>,
    mut ranges: [Option<&mut [f32]>; CHANNELS],
    cutoffs: &[f64],
    rate: f64,
    damping: f64,
    response: Response,
) {
    dispatch!(Level::new(), simd => {
        let mut gains = [0.0; SVF_CHUNK_FRAMES];
        let mut planes = ranges.each_mut().map(|range| {
            range.as_deref_mut().unwrap_or_default().chunks_mut(SVF_CHUNK_FRAMES)
        });
        for chunk in cutoffs.chunks(SVF_CHUNK_FRAMES) {
            platform::svf_gains(simd, chunk, rate, &mut gains);
            let mut frames = planes.each_mut().map(|plane| {
                plane.next().unwrap_or_default().iter_mut()
            });
            for gain in gains.iter().take(chunk.len()) {
                let mut samples = frames.each_mut().map(Iterator::next);
                state.frame_samples(simd, &mut samples, *gain, damping, response);
            }
        }
    });
}

pub(super) fn coefficients(gain: f64, damping: f64) -> [f64; 3] {
    let a1 = (1.0 + gain * (gain + damping)).recip();
    let a2 = gain * a1;
    let a3 = gain * a2;
    [a1, a2, a3]
}

#[cfg(test)]
mod recurrence_tests {
    use std::ops::DerefMut;

    use fearless_simd::{Level, Simd, dispatch};
    use kithara_test_utils::kithara;
    use num_traits::ToPrimitive;

    use super::{Response, State as StateVariableFilter};
    use crate::consts;

    trait Frame {
        fn frame<S: Simd, P: DerefMut<Target = [f32]>>(
            &mut self,
            simd: S,
            planes: &mut [P],
            frame: usize,
            gain: f64,
            damping: f64,
            response: Response,
        );
    }

    impl<const CHANNELS: usize> Frame for StateVariableFilter<CHANNELS> {
        fn frame<S: Simd, P: DerefMut<Target = [f32]>>(
            &mut self,
            simd: S,
            planes: &mut [P],
            frame: usize,
            gain: f64,
            damping: f64,
            response: Response,
        ) {
            let mut planes = planes.iter_mut();
            let mut samples: [Option<&mut f32>; CHANNELS] =
                std::array::from_fn(|_| planes.next().and_then(|plane| plane.get_mut(frame)));
            self.frame_samples(simd, &mut samples, gain, damping, response);
        }
    }

    struct Scalar<const CHANNELS: usize> {
        state: [[f64; 2]; CHANNELS],
        quiet: usize,
        reset: bool,
    }

    impl<const CHANNELS: usize> Scalar<CHANNELS> {
        fn frame(
            &mut self,
            planes: &mut [Vec<f32>],
            frame: usize,
            gain: f64,
            damping: f64,
            response: Response,
        ) {
            let mut quiet = true;
            if !(self.reset
                && planes
                    .iter()
                    .all(|plane| plane.get(frame).expect("oracle frame").to_bits() == 0))
            {
                let a1 = (1.0 + gain * (gain + damping)).recip();
                let a2 = gain * a1;
                let a3 = gain * a2;
                for (plane, state) in planes.iter_mut().zip(self.state.iter_mut()) {
                    let sample = plane.get_mut(frame).expect("oracle frame");
                    let input = *sample;
                    let x = f64::from(input);
                    let [s1, s2] = *state;
                    let v3 = x - s2;
                    let v1 = a1 * s1 + a2 * v3;
                    let v2 = s2 + a2 * s1 + a3 * v3;
                    *state = [2.0 * v1 - s1, 2.0 * v2 - s2];
                    let output = match response {
                        Response::LowPass => v2,
                        Response::HighPass => x - damping * v1 - v2,
                        Response::AllPass => x - 2.0 * damping * v1,
                    }
                    .to_f32()
                    .unwrap_or(f32::NAN);
                    quiet &=
                        input.abs() <= consts::SILENT_PEAK && output.abs() <= consts::SILENT_PEAK;
                    *sample = output;
                }
                self.reset = false;
            }
            self.quiet = if quiet {
                self.quiet.saturating_add(1).min(consts::SECTION_MEMORY)
            } else {
                0
            };
            if self.quiet == consts::SECTION_MEMORY {
                self.state.fill([0.0; 2]);
                self.reset = true;
            }
        }
    }

    fn ordered_recurrence<const CHANNELS: usize>(response: Response, damping: f64) {
        for level in [Level::fallback(), Level::new()] {
            let mut actual: Vec<Vec<f32>> = (0..CHANNELS)
                .map(|channel| {
                    let mut plane: Vec<_> = [0.0, -0.0, 0.75, -0.25, 1e-10, 0.0, 0.0]
                        .into_iter()
                        .cycle()
                        .take(513)
                        .collect();
                    if channel == 0 {
                        *plane.get_mut(257).expect("NaN witness frame") = f32::NAN;
                    }
                    plane
                })
                .collect();
            let mut expected = actual.clone();
            let mut filter = StateVariableFilter::<CHANNELS>::new();
            let mut scalar = Scalar {
                state: [[0.0; 2]; CHANNELS],
                quiet: 0,
                reset: true,
            };
            dispatch!(level, simd => {
                for (frame, gain) in [0.0, 1e-310, 0.01, 0.2, 1.0, 1e16]
                    .into_iter()
                    .cycle()
                    .take(513)
                    .enumerate()
                {
                    scalar.frame(&mut expected, frame, gain, damping, response);
                    filter.frame(simd, &mut actual, frame, gain, damping, response);
                    for (actual, expected) in actual.iter().zip(&expected) {
                        let (actual, expected) = (
                            actual.get(frame).expect("actual frame"),
                            expected.get(frame).expect("expected frame"),
                        );
                        if expected.is_nan() {
                            assert!(actual.is_nan(), "{response:?} at {frame}");
                        } else {
                            assert_eq!(actual.to_bits(), expected.to_bits(), "{response:?} at {frame}");
                        }
                    }
                    for (channel, expected) in scalar.state.iter().enumerate() {
                        for (actual, expected) in filter.state.iter().map(|plane| plane.get(channel).expect("state channel")).zip(expected) {
                            if expected.is_nan() {
                                assert!(actual.is_nan());
                            } else {
                                assert_eq!(actual.to_bits(), expected.to_bits());
                            }
                        }
                    }
                    assert_eq!(filter.quiet, scalar.quiet);
                    assert_eq!(filter.is_reset(), scalar.reset);
                }
            });
        }
    }

    #[kithara::test]
    #[case(Response::LowPass)]
    #[case(Response::HighPass)]
    #[case(Response::AllPass)]
    fn simd_recurrence_keeps_scalar_order_history_and_quiet_state(#[case] response: Response) {
        for damping in [0.1, 1.0, 10.0, f64::INFINITY] {
            ordered_recurrence::<1>(response, damping);
            ordered_recurrence::<2>(response, damping);
            ordered_recurrence::<3>(response, damping);
            ordered_recurrence::<6>(response, damping);
        }
    }
}
