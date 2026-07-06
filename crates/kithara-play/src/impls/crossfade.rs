use firewheel::dsp::fade::FadeCurve;

/// Default crossfade duration in seconds.
pub(crate) const DEFAULT_CROSSFADE_DURATION: f32 = 1.0;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub(crate) enum CrossfadeCurve {
    #[default]
    EqualPower,
}

pub(crate) fn map_curve(curve: CrossfadeCurve) -> FadeCurve {
    match curve {
        CrossfadeCurve::EqualPower => FadeCurve::SquareRoot,
    }
}

#[derive(Clone, Debug)]
pub(crate) struct CrossfadeSettings {
    pub(crate) curve: CrossfadeCurve,
    pub(crate) duration: f32,
}

impl Default for CrossfadeSettings {
    fn default() -> Self {
        Self {
            curve: CrossfadeCurve::default(),
            duration: DEFAULT_CROSSFADE_DURATION,
        }
    }
}

impl CrossfadeSettings {
    pub(crate) fn fade_curve(&self) -> FadeCurve {
        map_curve(self.curve)
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;

    #[kithara::test]
    fn map_curve_matches_expected_fade_curve() {
        assert_eq!(map_curve(CrossfadeCurve::EqualPower), FadeCurve::SquareRoot);
    }

    #[kithara::test]
    fn default_settings() {
        let settings = CrossfadeSettings::default();
        assert!((settings.duration - DEFAULT_CROSSFADE_DURATION).abs() < f32::EPSILON);
        assert_eq!(settings.curve, CrossfadeCurve::EqualPower);
        assert_eq!(settings.fade_curve(), FadeCurve::SquareRoot);
    }
}
