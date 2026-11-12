use std::{
    num::{NonZeroU32, NonZeroUsize},
    ops::{DerefMut, Range},
};

use kithara_bufpool::{HasPool, PoolError, SampleBuffer};
use kithara_config::LiveConfig;
use kithara_dsp::{
    filter::{Biquad, Coefficients, FilterError},
    param::{SmoothedParam64, SmootherConfig},
};
use kithara_signal::AudioSpec;

use super::{
    FilterConfig, FilterMotion, FilterParameter, FilterSettings, FilterSettingsChange, admission,
};
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

/// Causal pooled filtering with retained history and per-frame manual smoothing.
///
/// Steps use the shared default one-pole in f64 and settle each frame within
/// its declared fraction of the current step's distance.
#[derive(kithara_config::ConfigOwner)]
#[config_owner(settings)]
#[non_exhaustive]
pub struct FilterEffect {
    spec: AudioSpec,
    settings: FilterSettings,
    filter: Biquad,
    target_coefficients: Coefficients<f64>,
    cutoff: SmoothedParam64,
    q: SmoothedParam64,
    gain: SmoothedParam64,
    planes: Vec<SampleBuffer>,
    history_peak: f64,
}

impl FilterEffect {
    /// Prepare state and pooled scratch outside audio processing.
    /// # Errors
    /// Refuses invalid geometry, unrepresentable coefficients or insufficient pool budget.
    pub fn new<S: HasPool<f32>>(
        spec: AudioSpec,
        config: FilterConfig<S>,
    ) -> Result<Self, PrepareError> {
        let channels = NonZeroUsize::new(usize::from(spec.channels)).ok_or(PrepareError::Shape)?;
        let coefficients = config.coefficients(f64::from(spec.sample_rate.get()))?;
        let FilterConfig { pools, settings } = config;
        let mut filter = Biquad::new(channels, NonZeroUsize::MIN)?;
        filter.retune(0, coefficients)?;
        let planes = (0..channels.get())
            .map(|_| pools.get_with_len::<f32>(consts::FILTER_BLOCK_FRAMES))
            .collect::<Result<_, _>>()?;
        let smoothing = SmootherConfig::default();
        Ok(Self {
            spec,
            settings,
            filter,
            target_coefficients: coefficients,
            cutoff: SmoothedParam64::new(
                f64::from(settings.cutoff_hz()),
                0.0,
                smoothing,
                spec.sample_rate,
            ),
            q: SmoothedParam64::new(f64::from(settings.q()), 0.0, smoothing, spec.sample_rate),
            gain: SmoothedParam64::new(
                f64::from(f32::from(settings.gain_db())),
                0.0,
                smoothing,
                spec.sample_rate,
            ),
            planes,
            history_peak: 0.0,
        })
    }

    fn settled(&self) -> bool {
        self.cutoff.has_settled() && self.q.has_settled() && self.gain.has_settled()
    }

    fn process_frame(&mut self) {
        let mut peak = if self.tail_exhausted() {
            0.0
        } else {
            self.history_peak
        };
        for plane in &self.planes {
            peak = observe_peak(peak, plane[0]);
        }
        self.advance_parameters();
        self.filter
            .process(&mut self.planes, 0..1)
            .expect("prepared scratch frame");
        for plane in &self.planes {
            peak = observe_peak(peak, plane[0]);
        }
        self.history_peak = peak;
    }

    fn advance_parameters(&mut self) {
        if !self.settled() {
            for parameter in [&mut self.cutoff, &mut self.q, &mut self.gain] {
                if !parameter.has_settled() {
                    parameter.next_smoothed();
                    parameter.settle();
                }
            }
            let coefficients = if self.settled() {
                self.target_coefficients
            } else {
                self.settings
                    .coefficients_at(
                        f64::from(self.spec.sample_rate.get()),
                        self.cutoff.current_value(),
                        self.q.current_value(),
                        self.gain.current_value(),
                    )
                    .expect("admitted smoothing stays inside the physical parameter bounds")
            };
            self.filter
                .retune(0, coefficients)
                .expect("admitted smoothing box has stable f32 coefficients");
        }
    }

    /// Advance the parameter clock while an exhausted filter has no audio input.
    pub fn advance(&mut self, frames: usize) {
        for _ in 0..frames {
            if self.settled() {
                break;
            }
            self.advance_parameters();
        }
    }

    /// Summarize the pending manual transitions without advancing their clocks.
    /// # Errors
    /// Refuses an unrepresentable changing-frame bound.
    pub fn motions(&self, rate: NonZeroU32) -> Result<[FilterMotion; 3], FilterError> {
        let motions = FilterParameter::ALL.map(|field| {
            let mut parameter = self.parameter(field);
            if rate != self.spec.sample_rate {
                parameter.update_sample_rate(rate);
            }
            parameter_motion(field, parameter, rate)
        });
        let [cutoff, q, gain] = motions;
        Ok([cutoff?, q?, gain?])
    }

    /// Prepare one manual transition, optionally after its old programme's due-frame point.
    /// # Errors
    /// Refuses inconsistent fields or an unrepresentable motion bound.
    pub fn change_motion(
        &self,
        rate: NonZeroU32,
        change: FilterSettingsChange,
        prior: Option<FilterSettingsChange>,
    ) -> Result<FilterMotion, FilterError> {
        let field = FilterParameter::of(change);
        let current = match prior {
            Some(point) if FilterParameter::of(point) == field => f64::from(point),
            Some(_) => return Err(FilterError::Parameters),
            None => self.parameter(field).current_value(),
        };
        parameter_motion(field, transition(current, f64::from(change), rate), rate)
    }

    fn parameter(&self, field: FilterParameter) -> SmoothedParam64 {
        match field {
            FilterParameter::CutoffHz => self.cutoff,
            FilterParameter::Q => self.q,
            FilterParameter::GainDb => self.gain,
        }
    }

    /// Admit ordered, bounded movement before changing settings, clocks or history.
    /// # Errors
    /// Refuses unsafe coefficient or modulation amplification bounds.
    pub fn admit_trajectory(
        &self,
        rate: NonZeroU32,
        motions: &[FilterMotion; 3],
    ) -> Result<(), FilterError> {
        let mut motions = *motions;
        for field in FilterParameter::ALL {
            let motion = &mut motions[field.index()];
            if motion.field != field {
                return Err(FilterError::Parameters);
            }
            motion.include_current(self.parameter(field).current_value())?;
        }
        let peak = if self.tail_exhausted() {
            0.0
        } else {
            self.history_peak
        };
        admission::admit(self.settings, rate, &motions, peak)
    }

    /// Apply one manual target while preserving every other parameter's trajectory.
    /// # Errors
    /// Refuses a contextual design or smoothing box before changing state.
    pub fn configure(&mut self, change: FilterSettingsChange) -> Result<(), FilterError> {
        let mut settings = self.settings;
        settings.apply_change(change);
        if settings == self.settings {
            return Ok(());
        }
        let rate = self.spec.sample_rate;
        let coefficients = settings.coefficients(f64::from(rate.get()))?;
        settings.admit_smoothing(
            f64::from(rate.get()),
            self.cutoff.current_value(),
            self.q.current_value(),
            self.gain.current_value(),
        )?;
        let mut motions = self.motions(rate)?;
        motions[FilterParameter::of(change).index()] = self.change_motion(rate, change, None)?;
        self.admit_trajectory(rate, &motions)?;
        let (mut cutoff, mut q, mut gain) = (self.cutoff, self.q, self.gain);
        match change {
            FilterSettingsChange::CutoffHz(value) => {
                cutoff = transition(cutoff.current_value(), f64::from(value), rate);
            }
            FilterSettingsChange::Q(value) => {
                q = transition(q.current_value(), f64::from(value), rate);
            }
            FilterSettingsChange::GainDb(value) => {
                gain = transition(gain.current_value(), f64::from(f32::from(value)), rate);
            }
        }
        self.filter.retune(0, coefficients)?;
        self.settings = settings;
        self.target_coefficients = coefficients;
        self.cutoff = cutoff;
        self.q = q;
        self.gain = gain;
        Ok(())
    }

    /// Apply an already prepared trajectory point without another smoothing pass.
    /// # Errors
    /// Refuses an invalid contextual point without changing state.
    pub fn set_explicit(&mut self, change: FilterSettingsChange) -> Result<(), FilterError> {
        let mut settings = self.settings;
        settings.apply_change(change);
        let rate = self.spec.sample_rate;
        let coefficients = settings.coefficients(f64::from(rate.get()))?;
        settings.admit_smoothing(
            f64::from(rate.get()),
            self.cutoff.current_value(),
            self.q.current_value(),
            self.gain.current_value(),
        )?;
        let (mut cutoff, mut q, mut gain) = (self.cutoff, self.q, self.gain);
        let point = |value| SmoothedParam64::new(value, 0.0, SmootherConfig::default(), rate);
        match change {
            FilterSettingsChange::CutoffHz(value) => cutoff = point(f64::from(value)),
            FilterSettingsChange::Q(value) => q = point(f64::from(value)),
            FilterSettingsChange::GainDb(value) => gain = point(f64::from(f32::from(value))),
        }
        self.filter.retune(0, coefficients)?;
        self.settings = settings;
        self.target_coefficients = coefficients;
        self.cutoff = cutoff;
        self.q = q;
        self.gain = gain;
        Ok(())
    }

    /// Retune the resident filter at a new rate without allocating or resetting history.
    /// # Errors
    /// Refuses an unrepresentable design before changing the prepared rate or state.
    pub fn update_sample_rate(&mut self, rate: NonZeroU32) -> Result<(), PrepareError> {
        let spec = AudioSpec::new(self.spec.channels, rate);
        if self.accepts_spec(spec) {
            return Ok(());
        }
        let target = self.settings.coefficients(f64::from(rate.get()))?;
        self.settings.admit_smoothing(
            f64::from(rate.get()),
            self.cutoff.current_value(),
            self.q.current_value(),
            self.gain.current_value(),
        )?;
        self.admit_trajectory(rate, &self.motions(rate)?)?;
        let current = if self.settled() {
            target
        } else {
            self.settings.coefficients_at(
                f64::from(rate.get()),
                self.cutoff.current_value(),
                self.q.current_value(),
                self.gain.current_value(),
            )?
        };
        let (mut cutoff, mut q, mut gain) = (self.cutoff, self.q, self.gain);
        for parameter in [&mut cutoff, &mut q, &mut gain] {
            parameter.update_sample_rate(rate);
        }
        self.filter.retune(0, current)?;
        self.target_coefficients = target;
        self.cutoff = cutoff;
        self.q = q;
        self.gain = gain;
        self.spec = spec;
        Ok(())
    }

    /// Whether this filter is already prepared for the exact format.
    #[must_use]
    pub fn accepts_spec(&self, spec: AudioSpec) -> bool {
        self.spec == spec
    }

    /// Filter a validated range of every plane in place.
    /// # Errors
    /// Refuses a mismatched or incomplete plane range before advancing parameters or history.
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
        for frame in range {
            for (source, scratch) in planes.iter().zip(&mut self.planes) {
                scratch[0] = source[frame];
            }
            self.process_frame();
            for (destination, scratch) in planes.iter_mut().zip(&self.planes) {
                destination[frame] = scratch[0];
            }
        }
        Ok(())
    }

    /// Whether the existing retirement rule has exhausted the filter's tail.
    #[must_use]
    pub fn tail_exhausted(&self) -> bool {
        self.filter.is_reset()
    }

    /// Clear filter history and finish the current manual targets.
    /// # Panics
    /// Panics if the internal prepared-coefficient invariant is violated.
    pub fn reset(&mut self) {
        self.filter
            .retune(0, self.target_coefficients)
            .expect("prepared target coefficients");
        self.filter.reset();
        self.history_peak = 0.0;
        for parameter in [&mut self.cutoff, &mut self.q, &mut self.gain] {
            parameter.reset_to_target();
        }
    }
}

fn transition(current: f64, target: f64, rate: NonZeroU32) -> SmoothedParam64 {
    let mut parameter = SmoothedParam64::new(
        current,
        (target - current).abs(),
        SmootherConfig::default(),
        rate,
    );
    parameter.set_value(target);
    parameter
}

fn parameter_motion(
    field: FilterParameter,
    parameter: SmoothedParam64,
    rate: NonZeroU32,
) -> Result<FilterMotion, FilterError> {
    let (range, frames) = parameter.motion_bound().ok_or(FilterError::Parameters)?;
    let variation = 2.0 * FilterMotion::distance(field, rate, *range.start(), *range.end())?;
    FilterMotion::new(
        field,
        rate,
        range,
        parameter.current_value(),
        variation,
        frames,
        frames,
    )
}

fn observe_peak(peak: f64, value: f32) -> f64 {
    if value.is_finite() {
        peak.max(f64::from(value).abs())
    } else {
        f64::INFINITY
    }
}
