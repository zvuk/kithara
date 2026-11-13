use std::{
    num::NonZeroUsize,
    ops::{Deref, DerefMut, Range},
};

use num_traits::ToPrimitive;

use super::{Coefficients, FilterError};
use crate::{
    backend::{self, platform},
    consts,
};

/// Cascaded biquad sections applied in place to channel planes.
///
/// Sections start as identity and retain history between calls. Two consecutive
/// stream frames whose input and output both stay at or below `1e-9` reset
/// the state, so silence never decays into denormals.
pub struct Biquad {
    cascade: platform::Cascade,
    decay: Box<[usize]>,
    scratch: Box<[Box<[f32]>]>,
    quiet: usize,
    reset_state: bool,
}

impl Biquad {
    /// # Errors
    /// [`FilterError::Shape`] when `channels × sections` overflows,
    /// [`FilterError::Setup`] when the platform refuses the filter.
    pub fn new(channels: NonZeroUsize, sections: NonZeroUsize) -> Result<Self, FilterError> {
        Ok(Self {
            cascade: platform::Cascade::new(channels, sections)?,
            decay: vec![1; sections.get()].into_boxed_slice(),
            scratch: (0..channels.get())
                .map(|_| vec![0.0; consts::SETTLE_CHUNK].into_boxed_slice())
                .collect(),
            quiet: 0,
            reset_state: true,
        })
    }

    /// Replaces the coefficients of one section; the state carries over.
    ///
    /// # Errors
    /// [`FilterError::Shape`] when `section` is out of range,
    /// [`FilterError::Parameters`] when a coefficient is not finite in `f32`
    /// or a pole leaves the unit circle.
    pub fn retune(
        &mut self,
        section: usize,
        coefficients: Coefficients<f64>,
    ) -> Result<(), FilterError> {
        let slot = self.decay.get_mut(section).ok_or(FilterError::Shape)?;
        let values = in_f32(coefficients)?;
        self.cascade.set_section(section, values)?;
        *slot = decay(values);
        Ok(())
    }

    /// Filters `range` of every plane in place; an empty range is a no-op.
    ///
    /// # Errors
    /// [`FilterError::Shape`] when the plane count differs from the channel
    /// count or a plane does not cover `range`.
    pub fn process<P: DerefMut<Target = [f32]>>(
        &mut self,
        planes: &mut [P],
        range: Range<usize>,
    ) -> Result<(), FilterError> {
        if planes.len() != self.scratch.len()
            || planes
                .iter()
                .any(|plane| plane.get(range.clone()).is_none())
        {
            return Err(FilterError::Shape);
        }
        let mut cursor = range.start;
        while cursor < range.end {
            cursor = if self.quiet == 0 && !quiet(planes, cursor) {
                let end = backend::first_quiet(planes, cursor..range.end, consts::SILENT_PEAK);
                self.cascade.process(planes, cursor..end)?;
                self.reset_state = false;
                end
            } else if self.reset_state && positive_zero(planes, cursor) {
                let end = (cursor..range.end)
                    .find(|frame| !positive_zero(planes, *frame))
                    .unwrap_or(range.end);
                self.quiet = self
                    .quiet
                    .saturating_add(end.saturating_sub(cursor))
                    .min(consts::SECTION_MEMORY);
                end
            } else if self.quiet > 0 {
                let input_quiet = quiet(planes, cursor);
                let next = cursor.saturating_add(1);
                self.cascade.process(planes, cursor..next)?;
                self.count(input_quiet && quiet(planes, cursor));
                next
            } else {
                self.quiet_run(
                    planes,
                    cursor..range.end.min(cursor.saturating_add(consts::SETTLE_CHUNK)),
                )?
            };
        }
        Ok(())
    }

    fn quiet_run<P: DerefMut<Target = [f32]>>(
        &mut self,
        planes: &mut [P],
        run: Range<usize>,
    ) -> Result<usize, FilterError> {
        for (saved, plane) in self.scratch.iter_mut().zip(planes.iter()) {
            saved
                .get_mut(..run.len())
                .expect("prepared quiet scratch")
                .copy_from_slice(plane.get(run.clone()).expect("validated plane range"));
        }
        self.cascade.process(planes, run.clone())?;
        self.reset_state = false;
        for (offset, frame) in run.clone().enumerate() {
            if self.count(quiet(&self.scratch, offset) && quiet(planes, frame)) {
                let next = frame.saturating_add(1);
                for (saved, plane) in self.scratch.iter().zip(planes.iter_mut()) {
                    plane
                        .get_mut(next..run.end)
                        .expect("validated suffix")
                        .copy_from_slice(
                            saved
                                .get(offset.saturating_add(1)..run.len())
                                .expect("prepared scratch suffix"),
                        );
                }
                return Ok(next);
            }
        }
        Ok(run.end)
    }

    fn count(&mut self, frame_quiet: bool) -> bool {
        self.quiet = if frame_quiet {
            self.quiet.saturating_add(1).min(consts::SECTION_MEMORY)
        } else {
            0
        };
        self.reset_state = self.quiet == consts::SECTION_MEMORY;
        if self.reset_state {
            self.cascade.reset();
        }
        self.reset_state
    }

    /// Puts channel `c` in the steady state for the constant input `levels[c]`.
    ///
    /// # Errors
    /// [`FilterError::Shape`] when `levels` has another length than the
    /// channel count.
    pub fn settle(&mut self, levels: &[f32]) -> Result<(), FilterError> {
        if levels.len() != self.scratch.len() {
            return Err(FilterError::Shape);
        }
        self.cascade.reset();
        let frames = self
            .decay
            .iter()
            .fold(0_usize, |sum, frames| sum.saturating_add(*frames));
        for _ in 0..frames.div_ceil(consts::SETTLE_CHUNK).max(1) {
            for (plane, level) in self.scratch.iter_mut().zip(levels) {
                plane.fill(*level);
            }
            self.cascade
                .process(&mut self.scratch, 0..consts::SETTLE_CHUNK)?;
        }
        self.quiet = 0;
        self.reset_state = false;
        Ok(())
    }

    /// Takes over the state of `source`.
    ///
    /// # Errors
    /// [`FilterError::Shape`] when channel or section counts differ.
    pub fn copy_state(&mut self, source: &Self) -> Result<(), FilterError> {
        self.cascade.copy_state_from(&source.cascade)?;
        self.quiet = source.quiet;
        self.reset_state = source.reset_state;
        Ok(())
    }
    /// Whether construction, reset or quiet retirement left no residual history.
    #[must_use]
    pub fn is_reset(&self) -> bool {
        self.reset_state
    }

    /// Clears section history without changing coefficients or allocating.
    pub fn reset(&mut self) {
        self.cascade.reset();
        self.quiet = 0;
        self.reset_state = true;
    }
}

fn quiet<P: Deref<Target = [f32]>>(planes: &[P], frame: usize) -> bool {
    planes
        .iter()
        .all(|plane| plane.get(frame).expect("validated frame").abs() <= consts::SILENT_PEAK)
}

fn positive_zero<P: Deref<Target = [f32]>>(planes: &[P], frame: usize) -> bool {
    planes
        .iter()
        .all(|plane| plane.get(frame).expect("validated frame").to_bits() == 0)
}

/// `[b0, b1, b2, a1, a2]` rounded to `f32`, when every value stays finite
/// there and both poles of `1 + a1 z⁻¹ + a2 z⁻²` lie inside the unit circle.
fn in_f32(coefficients: Coefficients<f64>) -> Result<[f32; 5], FilterError> {
    let Coefficients { a1, a2, b0, b1, b2 } = coefficients;
    let values = [b0, b1, b2, a1, a2].map(|value| value.to_f32().unwrap_or(f32::NAN));
    let [_, _, _, a1, a2] = values;
    let stable = a2.abs() < 1.0 && a1.abs() < 1.0 + a2;
    if stable && values.iter().all(|value| value.is_finite()) {
        Ok(values)
    } else {
        Err(FilterError::Parameters)
    }
}

/// Frames the slowest pole of `coefficients` takes to decay under `floor`, a
/// level below one; at least one.
#[must_use]
pub fn decay_frames(coefficients: Coefficients<f64>, floor: f64) -> usize {
    frames_under(coefficients.a1, coefficients.a2, floor.log2())
}

/// Frames until the slowest pole of `values` decays below `2⁻²⁴`.
fn decay(values: [f32; 5]) -> usize {
    let [_, _, _, a1, a2] = values.map(f64::from);
    frames_under(a1, a2, consts::SETTLE_FLOOR_LOG2)
}

/// Frames until the slowest pole of `1 + a1 z⁻¹ + a2 z⁻²` decays below
/// `2^floor_log2`; at least one.
fn frames_under(a1: f64, a2: f64, floor_log2: f64) -> usize {
    let discriminant = a1.mul_add(a1, -4.0 * a2);
    let radius = if discriminant < 0.0 {
        a2.sqrt()
    } else {
        (a1.abs() + discriminant.sqrt()) * 0.5
    };
    (floor_log2 / radius.log2())
        .ceil()
        .to_usize()
        .map_or(1, |frames| frames.max(1))
}
