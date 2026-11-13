use std::{
    num::NonZeroU32,
    ops::{DerefMut, Range},
};

use super::FilterError;
use crate::backend::svf;

/// Frames one [`StateVariableFilter::process`] pass computes cutoff gains for;
/// callers that build per-frame cutoffs size their stack scratch to it.
pub const SVF_CHUNK_FRAMES: usize = 256;

/// The output a [`StateVariableFilter`] writes from its shared state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Response {
    LowPass,
    HighPass,
    AllPass,
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
    pub(crate) state: [[f64; CHANNELS]; 2],
    pub(crate) quiet: usize,
    pub(crate) reset_state: bool,
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
        let ranges: [Option<&mut [f32]>; CHANNELS] =
            std::array::from_fn(|_| planes.next().and_then(|plane| plane.get_mut(range.clone())));
        if ranges.iter().any(Option::is_none) {
            return Err(FilterError::Shape);
        }
        svf::process(self, ranges, cutoffs, rate, q.recip(), response);
        Ok(())
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

    pub(crate) fn clear(&mut self) {
        self.state.fill([0.0; CHANNELS]);
        self.reset_state = true;
    }
}
