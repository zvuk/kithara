use ordered_float::NotNan;

/// A positive rate approaching a target with an exponential time constant.
///
/// The time constant and elapsed inputs use the same units. A zero time
/// constant selects the target immediately. Evaluation rejects invalid times
/// and results outside the finite nonnegative range.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct ExponentialApproach {
    initial: NotNan<f64>,
    target: NotNan<f64>,
    time_constant: NotNan<f64>,
}

impl ExponentialApproach {
    /// Validates finite positive rates and a finite nonnegative time constant.
    #[must_use]
    pub fn new(initial: f64, target: f64, time_constant: f64) -> Option<Self> {
        if !initial.is_finite()
            || initial <= 0.0
            || !target.is_finite()
            || target <= 0.0
            || !time_constant.is_finite()
            || time_constant < 0.0
        {
            return None;
        }
        Some(Self {
            initial: NotNan::new(initial).ok()?,
            target: NotNan::new(target).ok()?,
            time_constant: NotNan::new(time_constant).ok()?,
        })
    }

    /// The rate at the beginning of the approach.
    #[must_use]
    pub fn initial(self) -> f64 {
        self.initial.into_inner()
    }

    /// The limiting rate, or the immediate rate for a zero time constant.
    #[must_use]
    pub fn target(self) -> f64 {
        self.target.into_inner()
    }

    /// The time constant in the units used by elapsed inputs.
    #[must_use]
    pub fn time_constant(self) -> f64 {
        self.time_constant.into_inner()
    }

    /// The instantaneous rate at a finite nonnegative elapsed time.
    #[must_use]
    pub fn rate_at(self, elapsed: f64) -> Option<f64> {
        if !elapsed.is_finite() || elapsed < 0.0 {
            return None;
        }
        let rate = if self.time_constant() == 0.0 {
            self.target()
        } else if self.initial() < self.target() {
            self.initial()
                - (self.target() - self.initial()) * (-elapsed / self.time_constant()).exp_m1()
        } else {
            self.target()
                + (self.initial() - self.target()) * (-elapsed / self.time_constant()).exp()
        };
        (rate.is_finite() && rate > 0.0).then_some(rate)
    }

    /// The integrated advance from zero through a finite nonnegative time.
    #[must_use]
    pub fn advance(self, elapsed: f64) -> Option<f64> {
        self.advance_between(0.0, elapsed)
    }

    /// Integrates an interval without subtracting large absolute advances.
    #[must_use]
    pub fn advance_between(self, start: f64, end: f64) -> Option<f64> {
        if !start.is_finite() || start < 0.0 || !end.is_finite() || end < start {
            return None;
        }
        self.advance_over(start, end - start)
    }

    /// Integrates a duration separately from its finite nonnegative start time.
    ///
    /// Keeping the duration separate preserves intervals smaller than the
    /// representable spacing of large absolute times.
    #[must_use]
    pub fn advance_over(self, start: f64, elapsed: f64) -> Option<f64> {
        if !start.is_finite() || start < 0.0 || !elapsed.is_finite() || elapsed < 0.0 {
            return None;
        }
        let advance = if elapsed == 0.0 {
            0.0
        } else if self.time_constant() == 0.0 || self.initial() == self.target() {
            self.target() * elapsed
        } else {
            self.target() * elapsed
                + (-start / self.time_constant()).exp() * self.transient_advance(elapsed)?
        };
        (advance.is_finite() && advance >= 0.0).then_some(advance)
    }

    /// The bounded signed contribution beyond the target's linear advance.
    pub(crate) fn transient_advance(self, elapsed: f64) -> Option<f64> {
        if !elapsed.is_finite() || elapsed < 0.0 {
            return None;
        }
        let advance = if elapsed == 0.0 || self.time_constant() == 0.0 {
            0.0
        } else {
            (self.initial() - self.target())
                * (-self.time_constant() * (-elapsed / self.time_constant()).exp_m1())
        };
        advance.is_finite().then_some(advance)
    }

    /// Scales both rates by a finite positive factor, preserving the time axis.
    #[must_use]
    pub fn scaled(self, factor: f64) -> Option<Self> {
        if !factor.is_finite() || factor <= 0.0 {
            return None;
        }
        Self::new(
            self.initial() * factor,
            self.target() * factor,
            self.time_constant(),
        )
    }
}
