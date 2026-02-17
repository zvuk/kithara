//! Sequential crossfade configuration and state tracking.
//!
//! Maps the rich [`CrossfadeCurve`] enum to firewheel's limited [`FadeCurve`],
//! and bundles duration + curve into [`CrossfadeSettings`] for the processor.

use crate::traits::dj::crossfade::CrossfadeCurve;
use firewheel::dsp::fade::FadeCurve;

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
    pub duration: f32,
    pub curve: CrossfadeCurve,
}

impl Default for CrossfadeSettings {
    fn default() -> Self {
        Self {
            duration: DEFAULT_CROSSFADE_DURATION,
            curve: CrossfadeCurve::default(),
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
    use super::*;

    #[test]
    fn equal_power_maps_to_square_root() {
        assert_eq!(map_curve(CrossfadeCurve::EqualPower), FadeCurve::SquareRoot);
    }

    #[test]
    fn linear_maps_to_linear() {
        assert_eq!(map_curve(CrossfadeCurve::Linear), FadeCurve::Linear);
    }

    #[test]
    fn s_curve_maps_to_linear() {
        assert_eq!(map_curve(CrossfadeCurve::SCurve), FadeCurve::Linear);
    }

    #[test]
    fn constant_power_maps_to_square_root() {
        assert_eq!(
            map_curve(CrossfadeCurve::ConstantPower),
            FadeCurve::SquareRoot
        );
    }

    #[test]
    fn fast_fade_in_maps_to_square_root() {
        assert_eq!(map_curve(CrossfadeCurve::FastFadeIn), FadeCurve::SquareRoot);
    }

    #[test]
    fn fast_fade_out_maps_to_square_root() {
        assert_eq!(
            map_curve(CrossfadeCurve::FastFadeOut),
            FadeCurve::SquareRoot
        );
    }

    #[test]
    fn default_settings() {
        let settings = CrossfadeSettings::default();
        assert!((settings.duration - DEFAULT_CROSSFADE_DURATION).abs() < f32::EPSILON);
        assert_eq!(settings.curve, CrossfadeCurve::EqualPower);
        assert_eq!(settings.fade_curve(), FadeCurve::SquareRoot);
    }
}
