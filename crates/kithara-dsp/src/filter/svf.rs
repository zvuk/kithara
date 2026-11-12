use std::{
    f64::consts::PI,
    num::{NonZeroU32, NonZeroUsize},
    ops::{DerefMut, Range},
};

use num_traits::ToPrimitive;

use super::FilterError;
use crate::consts;

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
/// State is `f64` per channel. Low-pass and high-pass are outputs of the same
/// state; at fixed `g` and `k` they equal the RBJ cookbook responses. Two
/// consecutive frames whose input and output both stay at or below `1e-9`
/// reset the state, so silence never decays into denormals.
pub struct StateVariableFilter {
    state: Box<[[f64; 2]]>,
    gains: Box<[f64]>,
    quiet: usize,
    reset_state: bool,
}

impl StateVariableFilter {
    #[must_use]
    pub fn new(channels: NonZeroUsize) -> Self {
        Self {
            state: vec![[0.0; 2]; channels.get()].into_boxed_slice(),
            gains: vec![0.0; consts::SVF_GAIN_CHUNK].into_boxed_slice(),
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
        if planes.len() != self.state.len()
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
        let mut frames = range;
        for chunk in cutoffs.chunks(self.gains.len()) {
            for (gain, cutoff) in self.gains.iter_mut().zip(chunk) {
                *gain = (PI * cutoff / rate).tan();
            }
            for (offset, frame) in frames.by_ref().take(chunk.len()).enumerate() {
                self.frame(planes, frame, offset, damping, response);
            }
        }
        Ok(())
    }

    fn frame<P: DerefMut<Target = [f32]>>(
        &mut self,
        planes: &mut [P],
        frame: usize,
        offset: usize,
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
        let gain = *self.gains.get(offset).expect("chunk fits the gain scratch");
        let a1 = (1.0 + gain * (gain + damping)).recip();
        let a2 = gain * a1;
        let a3 = gain * a2;
        let mut frame_quiet = true;
        for (plane, state) in planes.iter_mut().zip(self.state.iter_mut()) {
            let sample = plane.get_mut(frame).expect("validated frame");
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
            };
            let output = output.to_f32().unwrap_or(f32::NAN);
            frame_quiet &=
                input.abs() <= consts::SILENT_PEAK && output.abs() <= consts::SILENT_PEAK;
            *sample = output;
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
        self.state.fill([0.0; 2]);
        self.reset_state = true;
    }
}
