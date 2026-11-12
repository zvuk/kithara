use std::{num::NonZeroU32, ops::RangeInclusive};

use kithara_dsp::filter::FilterError;
use num_traits::ToPrimitive;

use super::FilterSettings;

/// Bounds supplied by the owner of one immutable knob trajectory.
#[derive(Clone, Copy, Debug)]
pub struct FilterMotion {
    rate: NonZeroU32,
    bounds: [f64; 2],
    start: f64,
    variation: f64,
    frames: u64,
    changing: u64,
}

impl FilterMotion {
    /// Validate a knob trajectory summary before it can change filter state.
    /// `variation` is the cutoff travel in warped log Hz at `rate`, as
    /// [`FilterSettings::travel`] measures it; `changing` bounds frames with a
    /// changing knob, including jumps.
    /// # Errors
    /// Refuses unordered/nonfinite bounds or inconsistent movement counts.
    pub fn new(
        rate: NonZeroU32,
        bounds: RangeInclusive<f64>,
        start: f64,
        variation: f64,
        frames: u64,
        changing: u64,
    ) -> Result<Self, FilterError> {
        if bounds.is_empty()
            || ![*bounds.start(), *bounds.end(), start, variation]
                .iter()
                .all(|v| v.is_finite())
            || !bounds.contains(&start)
            || variation < 0.0
            || changing > frames
            || (variation > 0.0 && changing == 0)
        {
            return Err(FilterError::Parameters);
        }
        Ok(Self {
            rate,
            bounds: [*bounds.start(), *bounds.end()],
            start,
            variation,
            frames,
            changing,
        })
    }

    /// Extends the motion with the jump from the knob value `current` to its start.
    pub(super) fn include_current(
        &mut self,
        settings: &FilterSettings,
        current: f64,
    ) -> Result<(), FilterError> {
        let jump = settings.travel(f64::from(self.rate.get()), current, self.start)?;
        self.variation += jump;
        self.bounds[0] = self.bounds[0].min(current);
        self.bounds[1] = self.bounds[1].max(current);
        if jump > 0.0 {
            self.changing = self
                .changing
                .checked_add(1)
                .ok_or(FilterError::Parameters)?;
            self.frames = self.frames.max(self.changing);
        }
        Ok(())
    }
}

pub(super) fn admit(
    settings: &FilterSettings,
    rate: NonZeroU32,
    motion: &FilterMotion,
    history_peak: f64,
) -> Result<(), FilterError> {
    if motion.rate != rate || !history_peak.is_finite() {
        return Err(FilterError::Parameters);
    }
    let rate = f64::from(rate.get());
    let Some(cutoff) = settings.cutoff_hull(motion.bounds[0], motion.bounds[1]) else {
        // Only the center: the stage passes the signal and holds no state.
        return Ok(());
    };
    let margin = settings.range_margin(rate, cutoff)?;
    let metric = Metric::new(rate, cutoff, settings.q().into())?;
    let frames = motion.frames.max(1);
    let coefficient_rounding = 2.0 * metric.rounding_error * count(motion.changing);
    let epsilon = f64::from(f32::EPSILON);
    let gamma = 9.0 * epsilon / (1.0 - 9.0 * epsilon);
    let feedback_error = 3.0 * gamma * metric.abs_u.cosh();
    let operation_error = feedback_error.ln_1p() * count(frames);
    let level = history_peak.max(1.0);
    let log_p = 0.5 * metric.abs_u.cosh().ln();
    let log_initial = level.ln() + 0.5 * std::f64::consts::LN_2 + 0.5 * metric.abs_u;
    let feedforward = 4.0 * (1.0 + epsilon) * (1.0 + gamma);
    let log_forcing = level.ln() + count(frames).ln() + feedforward.ln() + log_p;
    let prefix = log_add(log_initial, log_forcing)
        + log_p
        + 0.5 * (motion.variation + coefficient_rounding)
        + operation_error;
    // The final fixed section retains the existing kernel's static stability contract.
    let gap = margin * 0.5;
    let transient = prefix + (2.0 / gap).ln();
    let steady = level.ln() + feedforward.ln() + std::f64::consts::LN_2 - 2.0 * gap.ln();
    let bound = log_add(transient, steady);
    if bound.is_finite() && bound * (1.0 + epsilon) < f64::from(f32::MAX).ln() {
        Ok(())
    } else {
        Err(FilterError::Parameters)
    }
}

struct Metric {
    abs_u: f64,
    rounding_error: f64,
}

impl Metric {
    fn new(rate: f64, cutoff: [f64; 2], q: f64) -> Result<Self, FilterError> {
        let abs_u = cutoff
            .map(|cutoff| (std::f64::consts::PI * cutoff / rate).tan().ln().abs())
            .into_iter()
            .fold(0.0, f64::max);
        let sines = cutoff.map(|cutoff| (std::f64::consts::TAU * cutoff / rate).sin());
        let sine = if cutoff[0] <= rate * 0.25 && cutoff[1] >= rate * 0.25 {
            1.0
        } else {
            sines[0].max(sines[1])
        };
        let d_min = 2.0 / (1.0 + sine / (2.0 * q));
        let epsilon = f64::from(f32::EPSILON);
        if d_min <= 0.5 * epsilon {
            return Err(FilterError::Parameters);
        }
        let error_c = 1.5 * epsilon / (d_min - 0.5 * epsilon);
        let c = abs_u.tanh() + error_c;
        if c >= 1.0 || !c.is_finite() {
            return Err(FilterError::Parameters);
        }
        let u_error = error_c / (1.0 - c * c);
        Ok(Self {
            abs_u: abs_u + u_error,
            rounding_error: u_error,
        })
    }
}

fn log_add(left: f64, right: f64) -> f64 {
    let high = left.max(right);
    high + ((left - high).exp() + (right - high).exp()).ln()
}

fn count(value: u64) -> f64 {
    value.to_f64().unwrap_or(f64::INFINITY)
}
