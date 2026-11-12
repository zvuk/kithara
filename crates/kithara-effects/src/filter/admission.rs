use std::{num::NonZeroU32, ops::RangeInclusive};

use kithara_dsp::filter::FilterError;
use num_traits::ToPrimitive;

use super::{FilterParameter, FilterSettings, Kind};

/// Bounds supplied by the owner of one immutable parameter trajectory.
#[derive(Clone, Copy, Debug)]
pub struct FilterMotion {
    pub(super) field: FilterParameter,
    rate: NonZeroU32,
    pub(super) bounds: [f64; 2],
    pub(super) start: f64,
    variation: f64,
    frames: u64,
    changing: u64,
}

impl FilterMotion {
    /// Distance in the parameter's trajectory units, without advancing a filter.
    /// # Errors
    /// Refuses nonfinite values and nonpositive cutoff values.
    pub fn distance(
        field: FilterParameter,
        rate: NonZeroU32,
        left: f64,
        right: f64,
    ) -> Result<f64, FilterError> {
        distance(field, rate, left, right)
    }
    /// Validate a trajectory summary before it can change filter state.
    /// Cutoff variation is in warped log Hz at `rate`; Q and gain retain their units.
    /// `changing` bounds frames with changing parameters, including jumps.
    /// # Errors
    /// Refuses unordered/nonfinite bounds or inconsistent movement counts.
    pub fn new(
        field: FilterParameter,
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
            field,
            rate,
            bounds: [*bounds.start(), *bounds.end()],
            start,
            variation,
            frames,
            changing,
        })
    }

    pub(super) fn include_current(&mut self, current: f64) -> Result<(), FilterError> {
        let jump = distance(self.field, self.rate, current, self.start)?;
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

pub(super) fn distance(
    field: FilterParameter,
    rate: NonZeroU32,
    left: f64,
    right: f64,
) -> Result<f64, FilterError> {
    let value = if field == FilterParameter::CutoffHz {
        if left <= 0.0 || right <= 0.0 {
            return Err(FilterError::Parameters);
        }
        let rate = f64::from(rate.get());
        if left >= rate * 0.5 || right >= rate * 0.5 {
            return Err(FilterError::Parameters);
        }
        let left = (std::f64::consts::PI * left / rate).tan().ln();
        let right = (std::f64::consts::PI * right / rate).tan().ln();
        (right - left).abs()
    } else {
        (right - left).abs()
    };
    if value.is_finite() {
        Ok(value)
    } else {
        Err(FilterError::Parameters)
    }
}

pub(super) fn admit(
    settings: FilterSettings,
    rate: NonZeroU32,
    motions: &[FilterMotion; 3],
    history_peak: f64,
) -> Result<(), FilterError> {
    if motions.iter().any(|motion| motion.rate != rate) {
        return Err(FilterError::Parameters);
    }
    let [cutoff, q, gain] = motions.map(|motion| motion.bounds);
    let rate = f64::from(rate.get());
    let margin = settings.range_margin(rate, cutoff, q, gain)?;
    let metric = Metric::new(settings.kind(), rate, cutoff, q, gain)?;
    let changing = motions
        .iter()
        .try_fold(0_u64, |n, motion| n.checked_add(motion.changing))
        .ok_or(FilterError::Parameters)?;
    let frames = motions
        .iter()
        .map(|motion| motion.frames)
        .max()
        .unwrap_or(0)
        .max(1);
    let variation = motions[0].variation + metric.gain_scale * motions[2].variation;
    let coefficient_rounding = 2.0 * metric.rounding_error * count(changing);
    let epsilon = f64::from(f32::EPSILON);
    let gamma = 9.0 * epsilon / (1.0 - 9.0 * epsilon);
    let feedback_error = 3.0 * gamma * metric.abs_u.cosh();
    let operation_error = feedback_error.ln_1p() * count(frames);
    let level = history_peak.max(1.0);
    if !history_peak.is_finite() {
        return Err(FilterError::Parameters);
    }
    let log_p = 0.5 * metric.abs_u.cosh().ln();
    let log_initial = level.ln() + 0.5 * std::f64::consts::LN_2 + 0.5 * metric.abs_u;
    let feedforward = 4.0
        * (gain[1] * std::f64::consts::LN_10 / 20.0).exp().max(1.0)
        * (1.0 + epsilon)
        * (1.0 + gamma);
    let log_forcing = level.ln() + count(frames).ln() + feedforward.ln() + log_p;
    let prefix = log_add(log_initial, log_forcing)
        + log_p
        + 0.5 * (variation + coefficient_rounding)
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
    gain_scale: f64,
    rounding_error: f64,
}

impl Metric {
    fn new(
        kind: Kind,
        rate: f64,
        cutoff: [f64; 2],
        q: [f64; 2],
        gain: [f64; 2],
    ) -> Result<Self, FilterError> {
        let scale = match kind {
            Kind::LowShelf => -std::f64::consts::LN_10 / 80.0,
            Kind::HighShelf => std::f64::consts::LN_10 / 80.0,
            Kind::LowPass | Kind::HighPass => 0.0,
        };
        let corners = cutoff.into_iter().flat_map(|cutoff| {
            gain.map(|gain| (std::f64::consts::PI * cutoff / rate).tan().ln() + scale * gain)
        });
        let abs_u = corners.map(f64::abs).fold(0.0, f64::max);

        let sines = cutoff.map(|cutoff| (std::f64::consts::TAU * cutoff / rate).sin());
        let sine = if cutoff[0] <= rate * 0.25 && cutoff[1] >= rate * 0.25 {
            1.0
        } else {
            sines[0].max(sines[1])
        };
        let alpha = sine / (2.0 * q[0]);
        let d_min = if scale == 0.0 {
            2.0 / (1.0 + alpha)
        } else {
            let a = gain.map(|gain| (gain * std::f64::consts::LN_10 / 40.0).exp());
            4.0 * a[0].min(1.0) / (2.0 * a[1].max(1.0) + 2.0 * a[1].sqrt() * alpha)
        };
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
            gain_scale: scale.abs(),
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
