use std::{
    num::NonZeroUsize,
    ops::{Deref, DerefMut, Range},
};

use num_traits::ToPrimitive;

use super::{Coefficients, FilterError};
use crate::{
    backend::{self, Cascade},
    consts,
};

/// Cascaded biquad sections applied in place to channel planes.
///
/// Sections start as identity. The state carries over between calls, so a
/// stream split into any chunks yields the same samples. Two consecutive
/// frames whose input and output both stay at or below `1e-9` on every
/// channel reset the state; frames of exact zeros on a reset state pass
/// through untouched.
pub struct Biquad {
    cascade: Cascade,
    decay: Box<[usize]>,
    /// Settle input, and the input of a quiet run kept until its outputs
    /// show where the state resets.
    scratch: Box<[Box<[f32]>]>,
    /// Consecutive quiet frames just processed, up to `SECTION_MEMORY`.
    quiet: usize,
    /// The state is zero since the last reset.
    silent: bool,
}

impl Biquad {
    /// # Errors
    /// [`FilterError::Shape`] when `channels × sections` overflows.
    pub fn new(channels: NonZeroUsize, sections: NonZeroUsize) -> Result<Self, FilterError> {
        Ok(Self {
            cascade: Cascade::new(channels, sections)?,
            decay: vec![1; sections.get()].into_boxed_slice(),
            scratch: (0..channels.get())
                .map(|_| vec![0.0; consts::SETTLE_CHUNK].into_boxed_slice())
                .collect(),
            quiet: 0,
            silent: true,
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
        self.cascade.check(planes, &range)?;
        let mut cursor = range.start;
        while cursor < range.end {
            let rest = cursor..range.end;
            cursor = if self.quiet == 0 && !quiet(planes, cursor) {
                let sound = backend::first_quiet(planes, rest, consts::SILENT_PEAK);
                self.cascade.run(planes, cursor..sound);
                self.silent = false;
                sound
            } else if self.silent && zero(planes, cursor) {
                let skipped = rest.clone().find(|frame| !zero(planes, *frame));
                let end = skipped.unwrap_or(rest.end);
                self.quiet = self
                    .quiet
                    .saturating_add(end.saturating_sub(cursor))
                    .min(consts::SECTION_MEMORY);
                end
            } else if self.quiet > 0 {
                self.quiet_frame(planes, cursor)
            } else {
                self.quiet_run(
                    planes,
                    cursor..rest.end.min(cursor.saturating_add(consts::SETTLE_CHUNK)),
                )
            };
        }
        Ok(())
    }

    /// Filters `frame` and resets the state when it completes a quiet run.
    fn quiet_frame<P: DerefMut<Target = [f32]>>(
        &mut self,
        planes: &mut [P],
        frame: usize,
    ) -> usize {
        let input_quiet = quiet(planes, frame);
        let next = frame.saturating_add(1);
        self.cascade.run(planes, frame..next);
        self.count(input_quiet && quiet(planes, frame));
        next
    }

    /// Filters `run`, which starts with a quiet input frame, then rewinds to
    /// the first frame that resets the state: the frames after it keep their
    /// input and run again from the reset state.
    fn quiet_run<P: DerefMut<Target = [f32]>>(
        &mut self,
        planes: &mut [P],
        run: Range<usize>,
    ) -> usize {
        for (saved, plane) in self.scratch.iter_mut().zip(planes.iter()) {
            if let (Some(saved), Some(input)) = (saved.get_mut(..run.len()), plane.get(run.clone()))
            {
                saved.copy_from_slice(input);
            }
        }
        self.cascade.run(planes, run.clone());
        self.silent = false;
        for (offset, frame) in run.clone().enumerate() {
            let frame_quiet = quiet(&self.scratch, offset) && quiet(planes, frame);
            if self.count(frame_quiet) {
                let next = frame.saturating_add(1);
                let kept = offset.saturating_add(1)..run.len();
                for (saved, plane) in self.scratch.iter().zip(planes.iter_mut()) {
                    if let (Some(saved), Some(output)) =
                        (saved.get(kept.clone()), plane.get_mut(next..run.end))
                    {
                        output.copy_from_slice(saved);
                    }
                }
                return next;
            }
        }
        run.end
    }

    /// Counts a quiet frame; a full quiet run resets the state.
    fn count(&mut self, quiet: bool) -> bool {
        self.quiet = if quiet {
            self.quiet.saturating_add(1).min(consts::SECTION_MEMORY)
        } else {
            0
        };
        let reset = self.quiet == consts::SECTION_MEMORY;
        if reset {
            self.cascade.reset();
        }
        self.silent = reset;
        reset
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
        self.silent = false;
        Ok(())
    }

    /// Takes over the state of `source`.
    ///
    /// # Errors
    /// [`FilterError::Shape`] when channel or section counts differ.
    pub fn copy_state(&mut self, source: &Self) -> Result<(), FilterError> {
        self.cascade.copy_state_from(&source.cascade)?;
        self.quiet = source.quiet;
        self.silent = source.silent;
        Ok(())
    }
}

/// Every plane stays at or below the silence level at `frame`.
fn quiet<P: Deref<Target = [f32]>>(planes: &[P], frame: usize) -> bool {
    planes.iter().all(|plane| {
        plane
            .get(frame)
            .is_none_or(|sample| sample.abs() <= consts::SILENT_PEAK)
    })
}

/// Every plane holds an exact zero at `frame`.
fn zero<P: Deref<Target = [f32]>>(planes: &[P], frame: usize) -> bool {
    planes
        .iter()
        .all(|plane| plane.get(frame).is_none_or(|sample| *sample == 0.0))
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

/// Frames until the slowest pole of `values` decays below `2⁻²⁴`.
fn decay(values: [f32; 5]) -> usize {
    let [_, _, _, a1, a2] = values.map(f64::from);
    let discriminant = a1.mul_add(a1, -4.0 * a2);
    let radius = if discriminant < 0.0 {
        a2.sqrt()
    } else {
        (a1.abs() + discriminant.sqrt()) * 0.5
    };
    (consts::SETTLE_FLOOR_LOG2 / radius.log2())
        .ceil()
        .to_usize()
        .map_or(1, |frames| frames.max(1))
}
