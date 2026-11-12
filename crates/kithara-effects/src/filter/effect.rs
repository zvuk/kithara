use std::{
    num::{NonZeroU32, NonZeroUsize},
    ops::{DerefMut, Range},
};

use kithara_bufpool::{HasPool, PoolError, SampleBuffer};
use kithara_dsp::{
    filter::{Biquad, Coefficients, FilterError},
    param::{SmoothedParam64, SmootherConfig},
};
use kithara_signal::AudioSpec;

use super::{FilterConfig, FilterKnob, FilterMotion, FilterSettings, Kind, admission};
use crate::consts;

/// Why a filter's numerical state or pooled scratch could not be prepared.
#[derive(Debug, thiserror::Error)]
pub enum PrepareError {
    #[error("filter buffer geometry is unsupported")]
    Shape,
    #[error(transparent)]
    Filter(#[from] FilterError),
    #[error(transparent)]
    Pool(#[from] PoolError),
}

/// The deck filter: one bipolar knob over a low-pass and a high-pass response.
///
/// The center passes the signal bit for bit and holds no history. A manual
/// knob change glides with the shared one-pole in f64; a program sets the
/// knob per frame without smoothing. Changing the response side, including a
/// move through the center, restarts the filter from reset state.
#[derive(kithara_config::ConfigOwner)]
#[config_owner(settings)]
#[non_exhaustive]
pub struct FilterEffect {
    spec: AudioSpec,
    settings: FilterSettings,
    filter: Biquad,
    knob: SmoothedParam64,
    side: Option<Kind>,
    planes: Vec<SampleBuffer>,
    history_peak: f64,
}

impl FilterEffect {
    /// Prepare state and pooled scratch outside audio processing, at the center.
    /// # Errors
    /// Refuses invalid geometry or insufficient pool budget.
    pub fn new<S: HasPool<f32>>(
        spec: AudioSpec,
        config: FilterConfig<S>,
    ) -> Result<Self, PrepareError> {
        let channels = NonZeroUsize::new(usize::from(spec.channels)).ok_or(PrepareError::Shape)?;
        let FilterConfig { pools, settings } = config;
        let filter = Biquad::new(channels, NonZeroUsize::MIN)?;
        let planes = (0..channels.get())
            .map(|_| pools.get_with_len::<f32>(consts::FILTER_BLOCK_FRAMES))
            .collect::<Result<_, _>>()?;
        Ok(Self {
            spec,
            settings,
            filter,
            knob: SmoothedParam64::new(
                f64::from(FilterKnob::default()),
                0.0,
                SmootherConfig::default(),
                spec.sample_rate,
            ),
            side: None,
            planes,
            history_peak: 0.0,
        })
    }

    /// The knob value the filter currently runs at.
    #[must_use]
    pub fn knob(&self) -> f64 {
        self.knob.current_value()
    }

    /// Summarize the pending manual glide without advancing it.
    /// # Errors
    /// Refuses an unrepresentable changing-frame bound.
    pub fn motion(&self, rate: NonZeroU32) -> Result<FilterMotion, FilterError> {
        let mut knob = self.knob;
        if rate != self.spec.sample_rate {
            knob.update_sample_rate(rate);
        }
        self.glide_motion(knob, rate)
    }

    /// Prepare the motion of one manual glide from `prior`, or from the current knob.
    /// # Errors
    /// Refuses an unrepresentable motion bound.
    pub fn change_motion(
        &self,
        rate: NonZeroU32,
        knob: FilterKnob,
        prior: Option<FilterKnob>,
    ) -> Result<FilterMotion, FilterError> {
        let current = prior.map_or_else(|| self.knob.current_value(), f64::from);
        self.glide_motion(glide(current, f64::from(knob), rate), rate)
    }

    fn glide_motion(
        &self,
        knob: SmoothedParam64,
        rate: NonZeroU32,
    ) -> Result<FilterMotion, FilterError> {
        let (range, frames) = knob.motion_bound().ok_or(FilterError::Parameters)?;
        let travel = self
            .settings
            .travel(f64::from(rate.get()), *range.start(), *range.end())?;
        FilterMotion::new(
            rate,
            range,
            knob.current_value(),
            2.0 * travel,
            frames,
            frames,
        )
    }

    /// Admit one knob trajectory, starting from the current knob, before it changes state.
    /// # Errors
    /// Refuses unsafe coefficient or modulation amplification bounds.
    pub fn admit_trajectory(
        &self,
        rate: NonZeroU32,
        motion: FilterMotion,
    ) -> Result<(), FilterError> {
        let mut motion = motion;
        motion.include_current(&self.settings, self.knob.current_value())?;
        let peak = if self.tail_exhausted() {
            0.0
        } else {
            self.history_peak
        };
        admission::admit(&self.settings, rate, &motion, peak)
    }

    /// Glide to a manual knob target.
    /// # Errors
    /// Refuses an unsafe glide before changing state.
    pub fn configure(&mut self, knob: FilterKnob) -> Result<(), FilterError> {
        let rate = self.spec.sample_rate;
        let glide = glide(self.knob.current_value(), f64::from(knob), rate);
        self.admit_trajectory(rate, self.glide_motion(glide, rate)?)?;
        self.knob = glide;
        Ok(())
    }

    /// Run at a knob value of an admitted program, without smoothing.
    /// # Errors
    /// Refuses a value without a representable response before changing state.
    pub fn set_explicit(&mut self, knob: FilterKnob) -> Result<(), FilterError> {
        let value = f64::from(knob);
        let response = self
            .settings
            .coefficients(f64::from(self.spec.sample_rate.get()), value)?;
        self.knob.set_value(value);
        self.knob.reset_to_target();
        self.apply(response)
    }

    /// Retune the resident filter at a new rate without allocating or resetting history.
    /// # Errors
    /// Refuses an unsafe glide or unrepresentable response before changing state.
    pub fn update_sample_rate(&mut self, rate: NonZeroU32) -> Result<(), PrepareError> {
        let spec = AudioSpec::new(self.spec.channels, rate);
        if self.accepts_spec(spec) {
            return Ok(());
        }
        self.admit_trajectory(rate, self.motion(rate)?)?;
        let response = self
            .settings
            .coefficients(f64::from(rate.get()), self.knob.current_value())?;
        self.knob.update_sample_rate(rate);
        self.spec = spec;
        self.apply(response)?;
        Ok(())
    }

    /// Whether this filter is already prepared for the exact format.
    #[must_use]
    pub fn accepts_spec(&self, spec: AudioSpec) -> bool {
        self.spec == spec
    }

    /// Filter a validated range of every plane in place.
    /// # Errors
    /// Refuses a mismatched or incomplete plane range before advancing the knob or history.
    pub fn process<P: DerefMut<Target = [f32]>>(
        &mut self,
        planes: &mut [P],
        range: Range<usize>,
    ) -> Result<(), FilterError> {
        if planes.len() != self.planes.len()
            || planes
                .iter()
                .any(|plane| plane.get(range.clone()).is_none())
        {
            return Err(FilterError::Shape);
        }
        if self.side.is_none() && self.knob.has_settled() {
            return Ok(());
        }
        for frame in range {
            self.advance_knob()?;
            if self.side.is_none() {
                continue;
            }
            for (source, scratch) in planes.iter().zip(&mut self.planes) {
                scratch[0] = source[frame];
            }
            self.process_frame()?;
            for (destination, scratch) in planes.iter_mut().zip(&self.planes) {
                destination[frame] = scratch[0];
            }
        }
        Ok(())
    }

    fn process_frame(&mut self) -> Result<(), FilterError> {
        let mut peak = if self.tail_exhausted() {
            0.0
        } else {
            self.history_peak
        };
        for plane in &self.planes {
            peak = observe_peak(peak, plane[0]);
        }
        self.filter.process(&mut self.planes, 0..1)?;
        for plane in &self.planes {
            peak = observe_peak(peak, plane[0]);
        }
        self.history_peak = peak;
        Ok(())
    }

    fn advance_knob(&mut self) -> Result<(), FilterError> {
        if self.knob.has_settled() {
            return Ok(());
        }
        self.knob.next_smoothed();
        self.knob.settle();
        let response = self.settings.coefficients(
            f64::from(self.spec.sample_rate.get()),
            self.knob.current_value(),
        )?;
        self.apply(response)
    }

    fn apply(&mut self, response: Option<(Kind, Coefficients<f64>)>) -> Result<(), FilterError> {
        let side = response.map(|(kind, _)| kind);
        if side != self.side {
            self.filter.reset();
            self.history_peak = 0.0;
            self.side = side;
        }
        match response {
            Some((_, coefficients)) => self.filter.retune(0, coefficients),
            None => Ok(()),
        }
    }

    /// Advance a manual glide while an exhausted filter has no audio input.
    /// # Errors
    /// Refuses a knob value without a representable response.
    pub fn advance(&mut self, frames: usize) -> Result<(), FilterError> {
        for _ in 0..frames {
            if self.knob.has_settled() {
                break;
            }
            self.advance_knob()?;
        }
        Ok(())
    }

    /// Whether the existing retirement rule has exhausted the filter's tail.
    #[must_use]
    pub fn tail_exhausted(&self) -> bool {
        self.filter.is_reset()
    }

    /// Clear filter history and finish the current manual glide.
    /// # Errors
    /// Refuses a knob target without a representable response.
    pub fn reset(&mut self) -> Result<(), FilterError> {
        self.knob.reset_to_target();
        let response = self.settings.coefficients(
            f64::from(self.spec.sample_rate.get()),
            self.knob.current_value(),
        )?;
        self.apply(response)?;
        self.filter.reset();
        self.history_peak = 0.0;
        Ok(())
    }
}

fn glide(current: f64, target: f64, rate: NonZeroU32) -> SmoothedParam64 {
    let mut knob = SmoothedParam64::new(
        current,
        (target - current).abs(),
        SmootherConfig::default(),
        rate,
    );
    knob.set_value(target);
    knob
}

fn observe_peak(peak: f64, value: f32) -> f64 {
    if value.is_finite() {
        peak.max(f64::from(value).abs())
    } else {
        f64::INFINITY
    }
}
