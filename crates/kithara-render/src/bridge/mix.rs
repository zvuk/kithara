use kithara_config::Config;
use kithara_derive::Patch;
use kithara_signal::FaderValue;

/// A deck's mix level outside `0.0..=1.0`, or not a number.
#[derive(Clone, Copy, Debug, PartialEq, thiserror::Error)]
#[error("mix level {level} is not a finite value in 0.0..=1.0")]
pub struct InvalidMixLevel {
    pub level: f32,
}

/// How loud a deck sounds, changed while it plays.
///
/// A change of one field goes to the deck as one [`DeckPart::Mix`](super::DeckPart::Mix); the
/// deck applies it on its frame and ramps its output gain to [`DeckMixSettings::gain`].
#[derive(Clone, Copy, Debug, PartialEq, Config, Patch)]
#[config(default, check(error = InvalidMixLevel), fields(value, get(copy)))]
pub struct DeckMixSettings {
    /// The deck's fader, at unity unless changed.
    #[config(live, builder(default = FaderValue::DEFAULT))]
    volume: FaderValue,
    /// Whether the deck is silent whatever its volume, unmuted unless changed.
    #[config(live, builder(default))]
    muted: bool,
    /// The deck's share of the mix, a linear amplitude over its volume: the crossfader and
    /// the trim the deck's owner sets. At unity unless changed.
    #[config(live, check = check_level, builder(default = 1.0))]
    level: f32,
}

impl DeckMixSettings {
    /// The amplitude the deck sounds at: silence when muted, else the square of the fader
    /// times the mix level.
    #[must_use]
    pub fn gain(self) -> f32 {
        if self.muted {
            0.0
        } else {
            let volume = f32::from(self.volume);
            volume * volume * self.level
        }
    }
}

/// A mix level is a finite amplitude from silence to unity.
fn check_level(level: f32) -> Result<f32, InvalidMixLevel> {
    if level.is_finite() && (0.0..=1.0).contains(&level) {
        Ok(level)
    } else {
        Err(InvalidMixLevel { level })
    }
}
