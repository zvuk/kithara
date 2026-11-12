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
        let damping = q.recip();
        dispatch!(Level::new(), simd => {
            let mut gains = [0.0; SVF_CHUNK_FRAMES];
            let mut frames = range;
            for chunk in cutoffs.chunks(SVF_CHUNK_FRAMES) {
                platform::svf_gains(simd, chunk, rate, &mut gains[..chunk.len()]);
                for (gain, frame) in gains.iter().zip(frames.by_ref().take(chunk.len())) {
                    self.frame(simd, planes, frame, *gain, damping, response);
                }
            }
        });
        Ok(())
    }

    #[inline(always)]
    fn frame<S: Simd, P: DerefMut<Target = [f32]>>(
        &mut self,
        simd: S,
        planes: &mut [P],
        frame: usize,
        gain: f64,
        damping: f64,
        response: Response,
    ) {
        if self.reset_state
            && planes
                .iter()
                .all(|plane| plane.get(frame).expect("validated frame").to_bits() == 0)
        {
            self.count(true);
            return;
        }
        let a1 = (1.0 + gain * (gain + damping)).recip();
        let a2 = gain * a1;
        let a3 = gain * a2;
        let mut frame_quiet = true;
        let [memory1, memory2] = &mut self.state;
        for ((planes, memory1), memory2) in planes
            .chunks_mut(2)
            .zip(memory1.chunks_mut(2))
            .zip(memory2.chunks_mut(2))
        {
            let s1 = f64x2::from_fn(simd, |lane| memory1.get(lane).copied().unwrap_or(0.0));
            let s2 = f64x2::from_fn(simd, |lane| memory2.get(lane).copied().unwrap_or(0.0));
            let x = f64x2::from_fn(simd, |lane| {
                planes.get(lane).map_or(0.0, |plane| {
                    f64::from(*plane.get(frame).expect("validated frame"))
                })
            });
            let ([next1, next2], output) =
                svf_step(simd, x, [s1, s2], [a1, a2, a3], damping, response);
            memory1.copy_from_slice(&next1.as_slice()[..memory1.len()]);
            memory2.copy_from_slice(&next2.as_slice()[..memory2.len()]);
            for (plane, output) in planes.iter_mut().zip(output.as_slice()) {
                let sample = plane.get_mut(frame).expect("validated frame");
                let output = output.to_f32().unwrap_or(f32::NAN);
                frame_quiet &=
                    sample.abs() <= consts::SILENT_PEAK && output.abs() <= consts::SILENT_PEAK;
                *sample = output;
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
mod tests;
