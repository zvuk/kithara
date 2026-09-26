use kithara_dsp::fade::FadeCurve;

use crate::PlayError;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Deserialize)]
#[non_exhaustive]
pub enum SelectionPlayback {
    #[default]
    Play,
    Pause,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Deserialize)]
#[non_exhaustive]
pub enum CrossfadeCurve {
    /// Controls amplitude directly; unrelated tracks can have a perceived-power dip.
    Linear,
    /// Approximately preserves power for uncorrelated tracks, but correlated material can sum louder.
    #[default]
    EqualPower,
}

#[derive(Clone, Copy, Debug, PartialEq, serde::Deserialize)]
pub struct CrossfadeSettings {
    pub curve: CrossfadeCurve,
    pub depth: f32,
    pub duration: f32,
    pub position: f32,
}

impl Default for CrossfadeSettings {
    fn default() -> Self {
        Self {
            duration: 1.0,
            curve: CrossfadeCurve::EqualPower,
            depth: 1.0,
            position: 0.5,
        }
    }
}

impl CrossfadeSettings {
    pub fn new(
        duration: f32,
        curve: CrossfadeCurve,
        depth: f32,
        position: f32,
    ) -> Result<Self, PlayError> {
        let settings = Self {
            curve,
            depth,
            duration,
            position,
        };
        settings.validate()?;
        Ok(settings)
    }

    #[must_use]
    pub fn gains(self, progress: f32) -> (f32, f32) {
        let x = progress.clamp(0.0, 1.0);
        if x == 0.0 {
            return (1.0, 0.0);
        }
        if x == 1.0 {
            return (0.0, 1.0);
        }
        let u = if x <= self.position {
            x / (2.0 * self.position)
        } else {
            0.5 + (x - self.position) / (2.0 * (1.0 - self.position))
        };
        let linear = FadeCurve::Linear.compute_gains_0_to_1(u);
        let selected = FadeCurve::from(self.curve).compute_gains_0_to_1(u);
        (
            self.depth.mul_add(selected.0 - linear.0, linear.0),
            self.depth.mul_add(selected.1 - linear.1, linear.1),
        )
    }

    pub fn validate(self) -> Result<Self, PlayError> {
        if !self.duration.is_finite() || self.duration < 0.0 {
            return Err(PlayError::InvalidParameter {
                name: "crossfade.duration".into(),
                value: self.duration,
            });
        }
        if !self.depth.is_finite() || !(0.0..=1.0).contains(&self.depth) {
            return Err(PlayError::InvalidParameter {
                name: "crossfade.depth".into(),
                value: self.depth,
            });
        }
        if !self.position.is_finite() || self.position <= 0.0 || self.position >= 1.0 {
            return Err(PlayError::InvalidParameter {
                name: "crossfade.position".into(),
                value: self.position,
            });
        }
        Ok(self)
    }
}

impl From<CrossfadeCurve> for FadeCurve {
    fn from(curve: CrossfadeCurve) -> Self {
        match curve {
            CrossfadeCurve::Linear => Self::Linear,
            CrossfadeCurve::EqualPower => Self::EqualPower3dB,
        }
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;

    #[kithara::test]
    fn gain_law_keeps_endpoints_and_pivot_continuous() {
        let settings = CrossfadeSettings::new(2.0, CrossfadeCurve::EqualPower, 1.0, 0.3)
            .expect("valid settings");
        assert_eq!(settings.gains(0.0), (1.0, 0.0));
        assert_eq!(settings.gains(1.0), (0.0, 1.0));
        let pivot = settings.gains(0.3);
        let expected = 0.5_f32.sqrt();
        assert!((pivot.0 - expected).abs() < 1.0e-6);
        assert!((pivot.1 - expected).abs() < 1.0e-6);
        let left = settings.gains(0.3 - f32::EPSILON);
        let right = settings.gains(0.3 + f32::EPSILON);
        assert!((left.0 - right.0).abs() < 1.0e-5);
        assert!((left.1 - right.1).abs() < 1.0e-5);
    }

    #[kithara::test]
    fn depth_zero_is_linear_and_invalid_values_fail() {
        let settings = CrossfadeSettings::new(1.0, CrossfadeCurve::EqualPower, 0.0, 0.5)
            .expect("valid settings");
        assert_eq!(settings.gains(0.25), (0.75, 0.25));
        for invalid in [f32::NAN, f32::INFINITY, -1.0] {
            assert!(CrossfadeSettings::new(invalid, CrossfadeCurve::Linear, 1.0, 0.5).is_err());
        }
        assert!(CrossfadeSettings::new(1.0, CrossfadeCurve::Linear, 1.1, 0.5).is_err());
        assert!(CrossfadeSettings::new(1.0, CrossfadeCurve::Linear, 1.0, 0.0).is_err());
        assert!(CrossfadeSettings::new(1.0, CrossfadeCurve::Linear, 1.0, 1.0).is_err());
    }

    #[kithara::test]
    fn gain_law_is_monotonic_symmetric_and_supports_asymmetric_pivots() {
        let centred = CrossfadeSettings::new(1.0, CrossfadeCurve::EqualPower, 1.0, 0.5)
            .expect("valid settings");
        let mut previous = centred.gains(0.0);
        for step in 1..=100 {
            let gains = centred.gains(step as f32 / 100.0);
            assert!(gains.0 <= previous.0);
            assert!(gains.1 >= previous.1);
            previous = gains;
        }
        let quarter = centred.gains(0.25);
        let three_quarters = centred.gains(0.75);
        assert!((quarter.0 - three_quarters.1).abs() < 1.0e-6);
        assert!((quarter.1 - three_quarters.0).abs() < 1.0e-6);

        let early =
            CrossfadeSettings::new(1.0, CrossfadeCurve::Linear, 1.0, 0.25).expect("valid settings");
        assert_eq!(early.gains(0.25), (0.5, 0.5));
        assert_ne!(early.gains(0.25), centred.gains(0.25));

        let equal_power = CrossfadeSettings::new(1.0, CrossfadeCurve::EqualPower, 1.0, 0.5)
            .expect("valid settings");
        let linear =
            CrossfadeSettings::new(1.0, CrossfadeCurve::Linear, 1.0, 0.5).expect("valid settings");
        assert_ne!(equal_power.gains(0.25), linear.gains(0.25));
    }

    #[kithara::test]
    fn every_non_finite_or_out_of_range_field_is_rejected() {
        for invalid in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, -0.1] {
            assert!(CrossfadeSettings::new(invalid, CrossfadeCurve::Linear, 1.0, 0.5).is_err());
        }
        for invalid in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, -0.1, 1.1] {
            assert!(CrossfadeSettings::new(1.0, CrossfadeCurve::Linear, invalid, 0.5).is_err());
        }
        for invalid in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, 0.0, 1.0] {
            assert!(CrossfadeSettings::new(1.0, CrossfadeCurve::Linear, 1.0, invalid).is_err());
        }
    }
}
