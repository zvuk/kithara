use firewheel::dsp::fade::FadeCurve;

use crate::traits::dj::crossfade::CrossfadeCurve;

/// Default crossfade duration in seconds.
pub(crate) const DEFAULT_CROSSFADE_DURATION: f32 = 1.0;

/// Map our [`CrossfadeCurve`] to firewheel's [`FadeCurve`].
///
/// Firewheel only supports `Linear` and `SquareRoot`.
/// Our richer enum maps to the closest available:
/// - `EqualPower`, `ConstantPower`, `FastFadeIn`, `FastFadeOut` -> `SquareRoot`
/// - `Linear`, `SCurve` -> `Linear`
pub(crate) fn map_curve(curve: CrossfadeCurve) -> FadeCurve {
    match curve {
        CrossfadeCurve::Linear | CrossfadeCurve::SCurve => FadeCurve::Linear,
        _ => FadeCurve::SquareRoot,
    }
}

/// Crossfade configuration for the player processor.
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
    /// Get the mapped firewheel [`FadeCurve`].
    pub(crate) fn fade_curve(&self) -> FadeCurve {
        map_curve(self.curve)
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;

    #[kithara::test]
    #[case(CrossfadeCurve::EqualPower, FadeCurve::SquareRoot)]
    #[case(CrossfadeCurve::Linear, FadeCurve::Linear)]
    #[case(CrossfadeCurve::SCurve, FadeCurve::Linear)]
    #[case(CrossfadeCurve::ConstantPower, FadeCurve::SquareRoot)]
    #[case(CrossfadeCurve::FastFadeIn, FadeCurve::SquareRoot)]
    #[case(CrossfadeCurve::FastFadeOut, FadeCurve::SquareRoot)]
    fn map_curve_matches_expected_fade_curve(
        #[case] input: CrossfadeCurve,
        #[case] expected: FadeCurve,
    ) {
        assert_eq!(map_curve(input), expected);
    }

    #[kithara::test]
    fn default_settings() {
        let settings = CrossfadeSettings::default();
        assert!((settings.duration - DEFAULT_CROSSFADE_DURATION).abs() < f32::EPSILON);
        assert_eq!(settings.curve, CrossfadeCurve::EqualPower);
        assert_eq!(settings.fade_curve(), FadeCurve::SquareRoot);
    }
}
