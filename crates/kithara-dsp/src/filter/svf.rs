use std::{
    num::NonZeroU32,
    ops::{DerefMut, Range},
};

use fearless_simd::{Level, dispatch, f64x2, prelude::*};
use num_traits::ToPrimitive;

use super::FilterError;
use crate::{
    backend::{platform, svf_step},
    consts,
};

/// Frames one [`StateVariableFilter::process`] pass computes cutoff gains for;
/// callers that build per-frame cutoffs size their stack scratch to it.
pub const SVF_CHUNK_FRAMES: usize = 256;

/// The output a [`StateVariableFilter`] writes from its shared state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Response {
    LowPass,
    HighPass,
}

/// Second-order TPT state-variable filter applied in place to channel planes.
///
/// Trapezoidal integrators in transposed direct form II carry the cutoff gain
/// `g = tan(π·fc/fs)` before each integrator; `k = 1/Q` damps the band-pass
/// feedback. The filter takes one cutoff per frame and computes `g` for a
/// chunk of frames in one pass. One frame solves `y = (I − g·A)⁻¹·(s + g·b·x)` for
/// `A = [[−k, −1], [1, 0]]` and stores `s' = 2·y − s`, so the state follows the
/// Cayley transform of `g·A`. Since `A + Aᵀ ≤ 0`, that step never grows the
/// state for any `g ≥ 0` and `k ≥ 0`, and `g` may change on every frame.
///
/// State is two `f64` planes, `CHANNELS` wide, held inline and processed in
/// channel pairs. Gain preparation uses Accelerate on Apple and portable SIMD
/// elsewhere; its scratch is on the stack, so nothing allocates.
/// Low-pass and high-pass are outputs of the same
/// state; at fixed `g` and `k` they equal the RBJ cookbook responses. Two
/// consecutive frames whose input and output both stay at or below `1e-9`
/// reset the state, so silence never decays into denormals.
pub struct StateVariableFilter<const CHANNELS: usize> {
    state: [[f64; CHANNELS]; 2],
    quiet: usize,
    reset_state: bool,
}

impl<const CHANNELS: usize> Default for StateVariableFilter<CHANNELS> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const CHANNELS: usize> StateVariableFilter<CHANNELS> {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            state: [[0.0; CHANNELS]; 2],
            quiet: 0,
            reset_state: true,
        }
    }

    /// Filters `range` of every plane in place with one cutoff in Hz per frame.
    ///
    /// # Errors
    /// [`FilterError::Shape`] when the plane count differs from the channel
    /// count, a plane does not cover `range` or `cutoffs` has another length;
    /// [`FilterError::Parameters`] when `q` is not positive and finite or a
    /// cutoff is not positive or reaches the Nyquist frequency of `rate`. A
    /// refused call changes no sample and no state.
    pub fn process<P: DerefMut<Target = [f32]>>(
        &mut self,
        planes: &mut [P],
        range: Range<usize>,
        cutoffs: &[f64],
        rate: NonZeroU32,
        q: f64,
        response: Response,
    ) -> Result<(), FilterError> {
        if planes.len() != CHANNELS
            || cutoffs.len() != range.len()
            || planes
                .iter()
                .any(|plane| plane.get(range.clone()).is_none())
        {
            return Err(FilterError::Shape);
        }
        let rate = f64::from(rate.get());
        let nyquist = rate / 2.0;
        if !(q.is_finite() && q > 0.0)
            || !cutoffs
                .iter()
                .all(|cutoff| *cutoff > 0.0 && *cutoff < nyquist)
        {
            return Err(FilterError::Parameters);
        }
        let mut planes = planes.iter_mut();
        let mut ranges: [Option<&mut [f32]>; CHANNELS] =
            std::array::from_fn(|_| planes.next().and_then(|plane| plane.get_mut(range.clone())));
        if ranges.iter().any(Option::is_none) {
            return Err(FilterError::Shape);
        }
        let damping = q.recip();
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
                    self.frame_samples(simd, &mut samples, *gain, damping, response);
                }
            }
        });
        Ok(())
    }

    #[inline(always)]
    fn frame_samples<S: Simd>(
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
        let a1 = (1.0 + gain * (gain + damping)).recip();
        let a2 = gain * a1;
        let a3 = gain * a2;
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
            let ([next1, next2], output) =
                svf_step(simd, x, [s1, s2], [a1, a2, a3], damping, response);
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

    /// Whether construction, reset or quiet retirement left no residual history.
    #[must_use]
    pub fn is_reset(&self) -> bool {
        self.reset_state
    }

    /// Clears the state without allocating.
    pub fn reset(&mut self) {
        self.quiet = 0;
        self.clear();
    }

    fn clear(&mut self) {
        self.state.fill([0.0; CHANNELS]);
        self.reset_state = true;
    }
}

#[cfg(test)]
mod tests {
    use std::ops::DerefMut;

    use fearless_simd::{Level, Simd, dispatch};
    use kithara_test_utils::kithara;
    use num_traits::ToPrimitive;

    use super::{Response, StateVariableFilter};
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
    fn simd_recurrence_keeps_scalar_order_history_and_quiet_state(#[case] response: Response) {
        for damping in [0.1, 1.0, 10.0, f64::INFINITY] {
            ordered_recurrence::<1>(response, damping);
            ordered_recurrence::<2>(response, damping);
            ordered_recurrence::<3>(response, damping);
            ordered_recurrence::<6>(response, damping);
        }
    }
}
