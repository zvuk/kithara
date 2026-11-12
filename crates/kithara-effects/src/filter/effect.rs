use std::{
    num::NonZeroU32,
    ops::{DerefMut, Range},
};

use kithara_dsp::{
    filter::{FilterError, Response, SVF_CHUNK_FRAMES, StateVariableFilter},
    param::{SmoothedParam64, SmootherConfig},
};

use super::{FilterKnob, FilterSettings, Kind};
use crate::consts;

/// The deck filter: one bipolar knob over the low-pass and high-pass outputs
/// of one stereo state-variable filter.
///
/// The center passes the signal bit for bit and holds no history. A manual
/// knob change glides with the shared one-pole in f64; a program sets the
/// knob on every frame without smoothing. The filter is stable for any knob
/// motion, so no trajectory is checked. Changing the response side, including
/// a move through the center, restarts the filter from reset state.
#[derive(kithara_config::ConfigOwner)]
#[config_owner(settings)]
#[non_exhaustive]
pub struct FilterEffect {
    rate: NonZeroU32,
    settings: FilterSettings,
    knob: SmoothedParam64,
    stage: Stage,
}

impl FilterEffect {
    /// Prepare the filter at `rate`, at the center.
    #[must_use]
    pub fn new(rate: NonZeroU32, settings: FilterSettings) -> Self {
        Self {
            rate,
            settings,
            knob: SmoothedParam64::new(
                f64::from(FilterKnob::default()),
                0.0,
                SmootherConfig::default(),
                rate,
            ),
            stage: Stage {
                filter: StateVariableFilter::new(),
                side: None,
            },
        }
    }

    /// The knob value the filter currently runs at.
    #[must_use]
    pub fn knob(&self) -> f64 {
        self.knob.current_value()
    }

    /// Glide to a manual knob target from the current knob value.
    pub fn configure(&mut self, knob: FilterKnob) {
        let current = self.knob.current_value();
        let target = f64::from(knob);
        self.knob = SmoothedParam64::new(
            current,
            (target - current).abs(),
            SmootherConfig::default(),
            self.rate,
        );
        self.knob.set_value(target);
    }

    /// Retune at a new rate without allocating or resetting history; the knob
    /// map recomputes its cutoffs for that rate.
    pub fn update_sample_rate(&mut self, rate: NonZeroU32) {
        if rate != self.rate {
            self.knob.update_sample_rate(rate);
            self.rate = rate;
        }
    }

    /// Filter a validated range of every plane in place under the manual knob.
    /// # Errors
    /// Refuses a mismatched or incomplete plane range before advancing the knob or history.
    pub fn process<P: DerefMut<Target = [f32]>>(
        &mut self,
        planes: &mut [P],
        range: Range<usize>,
    ) -> Result<(), FilterError> {
        Self::check_planes(planes, &range)?;
        if self.stage.side.is_none() && self.knob.has_settled() {
            return Ok(());
        }
        let mut glide = [0.0; SVF_CHUNK_FRAMES];
        let mut start = range.start;
        while start < range.end {
            let end = range.end.min(start + SVF_CHUNK_FRAMES);
            let knobs = &mut glide[..end - start];
            for knob in knobs.iter_mut() {
                if !self.knob.has_settled() {
                    self.knob.next_smoothed();
                    self.knob.settle();
                }
                *knob = self.knob.current_value();
            }
            self.stage
                .run(&self.settings, self.rate, planes, start..end, knobs)?;
            start = end;
        }
        Ok(())
    }

    /// Filter a validated range under one program knob value per frame.
    ///
    /// The knob ends at the last value; a running manual glide ends with it.
    /// # Errors
    /// Refuses a mismatched plane range, a knob slice of another length or a
    /// value outside the knob range, before changing any sample or state.
    pub fn process_program<P: DerefMut<Target = [f32]>>(
        &mut self,
        planes: &mut [P],
        range: Range<usize>,
        knobs: &[f64],
    ) -> Result<(), FilterError> {
        Self::check_planes(planes, &range)?;
        if knobs.len() != range.len() {
            return Err(FilterError::Shape);
        }
        if !knobs
            .iter()
            .all(|knob| FilterKnob::checked(*knob).is_some())
        {
            return Err(FilterError::Parameters);
        }
        let Some(last) = knobs.last() else {
            return Ok(());
        };
        self.stage
            .run(&self.settings, self.rate, planes, range, knobs)?;
        self.knob.set_value(*last);
        self.knob.reset_to_target();
        Ok(())
    }

    fn check_planes<P: DerefMut<Target = [f32]>>(
        planes: &[P],
        range: &Range<usize>,
    ) -> Result<(), FilterError> {
        if planes.len() == consts::STEREO.get()
            && planes
                .iter()
                .all(|plane| plane.get(range.clone()).is_some())
        {
            Ok(())
        } else {
            Err(FilterError::Shape)
        }
    }

    /// Advance a manual glide while an exhausted filter has no audio input.
    pub fn advance(&mut self, frames: usize) {
        for _ in 0..frames {
            if self.knob.has_settled() {
                break;
            }
            self.knob.next_smoothed();
            self.knob.settle();
        }
        let side = self
            .settings
            .response(self.rate, self.knob.current_value())
            .map(|(kind, _)| kind);
        self.stage.enter(side);
    }

    /// Whether the filter holds no history: at the center, or after reset or a quiet tail.
    #[must_use]
    pub fn tail_exhausted(&self) -> bool {
        self.stage.side.is_none() || self.stage.filter.is_reset()
    }

    /// Clear filter history and finish the current manual glide.
    pub fn reset(&mut self) {
        self.knob.reset_to_target();
        self.stage.filter.reset();
        self.stage.side = self
            .settings
            .response(self.rate, self.knob.current_value())
            .map(|(kind, _)| kind);
    }
}

/// The stereo filter state and the side it runs on.
struct Stage {
    filter: StateVariableFilter<{ consts::STEREO.get() }>,
    side: Option<Kind>,
}

impl Stage {
    fn enter(&mut self, side: Option<Kind>) {
        if side != self.side {
            self.filter.reset();
            self.side = side;
        }
    }

    /// Runs validated `range` of `planes` with one knob value per frame.
    fn run<P: DerefMut<Target = [f32]>>(
        &mut self,
        settings: &FilterSettings,
        rate: NonZeroU32,
        planes: &mut [P],
        range: Range<usize>,
        knobs: &[f64],
    ) -> Result<(), FilterError> {
        let q = f64::from(settings.q());
        let mut cutoffs = [0.0; SVF_CHUNK_FRAMES];
        let mut offset = 0;
        while offset < knobs.len() {
            let side = settings.response(rate, knobs[offset]).map(|(kind, _)| kind);
            self.enter(side);
            let limit = knobs.len().min(offset + SVF_CHUNK_FRAMES);
            let mut end = offset;
            while end < limit {
                match settings.response(rate, knobs[end]) {
                    Some((kind, cutoff)) if Some(kind) == side => {
                        cutoffs[end - offset] = cutoff;
                    }
                    None if side.is_none() => {}
                    _ => break,
                }
                end += 1;
            }
            if let Some(kind) = side {
                let response = match kind {
                    Kind::LowPass => Response::LowPass,
                    Kind::HighPass => Response::HighPass,
                };
                self.filter.process(
                    planes,
                    range.start + offset..range.start + end,
                    &cutoffs[..end - offset],
                    rate,
                    q,
                    response,
                )?;
            }
            offset = end;
        }
        Ok(())
    }
}
