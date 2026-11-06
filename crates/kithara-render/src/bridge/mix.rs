use kithara_config::Config;
use kithara_derive::{Patch, Ranged};
use kithara_signal::FaderValue;

/// The linear amplitude a deck's slot sum enters its chain at, before its
/// equaliser and FX units.
#[derive(Clone, Copy, Debug, PartialEq, Ranged)]
#[ranged(min = 0.0, max = 1.0, default = 1.0)]
pub struct Trim(f32);

/// The crossfader's share of a deck's output, a linear amplitude after its
/// FX units.
#[derive(Clone, Copy, Debug, PartialEq, Ranged)]
#[ranged(min = 0.0, max = 1.0, default = 1.0)]
pub struct CrossfaderGain(f32);

/// How loud a deck sounds, changed while it plays.
///
/// A change of one field goes to the deck as one [`DeckPart::Mix`](super::DeckPart::Mix); the
/// deck applies it on its frame and ramps its trim to [`DeckMixSettings::trim`] and its output
/// to [`DeckMixSettings::output_gain`].
#[derive(Clone, Copy, Debug, PartialEq, Config, Patch)]
#[config(default, fields(value, get(copy)))]
pub struct DeckMixSettings {
    /// The deck's fader, at unity unless changed.
    #[config(live, builder(default = FaderValue::DEFAULT))]
    volume: FaderValue,
    /// Whether the deck is silent whatever its volume, unmuted unless changed.
    #[config(live, builder(default))]
    muted: bool,
    /// The deck's trim, at unity unless changed.
    #[config(live, builder(default = Trim::DEFAULT))]
    trim: Trim,
    /// The crossfader's share of the deck, at unity unless changed.
    #[config(live, builder(default = CrossfaderGain::DEFAULT))]
    crossfader: CrossfaderGain,
}

impl DeckMixSettings {
    /// The amplitude the deck's output sounds at after its FX units: silence
    /// when muted, else the square of the fader times the crossfader's share.
    #[must_use]
    pub fn output_gain(self) -> f32 {
        if self.muted {
            0.0
        } else {
            let volume = f32::from(self.volume);
            volume * volume * f32::from(self.crossfader)
        }
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::{CrossfaderGain, Trim};
    use crate::bridge::FxMix;

    #[kithara::test]
    fn deck_controls_hold_only_finite_amplitudes_from_silence_to_unity() {
        for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, 1.5, -0.1] {
            assert_eq!(Trim::checked(bad), None, "trim {bad}");
            assert_eq!(CrossfaderGain::checked(bad), None, "crossfader {bad}");
            assert_eq!(FxMix::checked(bad), None, "dry/wet {bad}");
        }
        for edge in [0.0, 1.0] {
            assert_eq!(Trim::checked(edge).map(f32::from), Some(edge));
            assert_eq!(CrossfaderGain::checked(edge).map(f32::from), Some(edge));
            assert_eq!(FxMix::checked(edge).map(f32::from), Some(edge));
        }
    }
}
