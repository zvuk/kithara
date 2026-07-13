use kithara::play::{CrossfaderBus, PlayError, PlayerImpl, apply_mix, crossfader_gain};
use kithara_platform::sync::Arc;

/// One deck's contribution to the shared session mix. `player` is a deck's
/// `PlayerImpl`; `bus` assigns it to a crossfader side (or `Bypass` for an
/// ordinary fader). `trim` and `muted` are per-deck level controls applied
/// before the crossfader.
pub struct DeckStrip {
    pub player: Arc<PlayerImpl>,
    pub bus: CrossfaderBus,
    pub trim: f32,
    pub muted: bool,
}

impl DeckStrip {
    #[must_use]
    pub fn new(player: Arc<PlayerImpl>, bus: CrossfaderBus) -> Self {
        Self {
            player,
            bus,
            trim: 1.0,
            muted: false,
        }
    }
}

/// App-owned mix state for the shared session. It is the single owner of the
/// app's desired mix and commits it through the common `SessionMixer`; it holds
/// no applied graph state of its own. The crossfader curve and session actuation
/// come from `kithara-play`, never reimplemented here.
pub struct DeckMix {
    pub position: f32,
    pub group_master: f32,
    pub decks: Vec<DeckStrip>,
}

impl DeckMix {
    #[must_use]
    pub fn new(decks: Vec<DeckStrip>) -> Self {
        Self {
            position: 0.5,
            group_master: 1.0,
            decks,
        }
    }

    /// Resolve each deck's final session-input level and actuate the whole group
    /// in one session batch.
    ///
    /// # Errors
    /// Returns [`PlayError`] if the crossfader position is invalid or the
    /// session rejects the batch (foreign session, duplicate player, invalid
    /// level).
    pub fn apply(&self) -> Result<(), PlayError> {
        let mut levels: Vec<(&PlayerImpl, f32)> = Vec::with_capacity(self.decks.len());
        for deck in &self.decks {
            let mute = if deck.muted { 0.0 } else { 1.0 };
            let level = resolve_level(deck.bus, deck.trim, mute, self.position, self.group_master)?;
            levels.push((deck.player.as_ref(), level));
        }
        apply_mix(levels)
    }
}

/// `final = trim * mute * crossfader_gain(bus, position) * group_master`, all
/// normalized factors. The crossfader coefficient is the common `kithara-play`
/// policy, not a local curve.
fn resolve_level(
    bus: CrossfaderBus,
    trim: f32,
    mute: f32,
    position: f32,
    group_master: f32,
) -> Result<f32, PlayError> {
    let gain = crossfader_gain(bus, position)?;
    Ok((trim * mute * gain * group_master).clamp(0.0, 1.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_b_endpoints_resolve_to_full_and_silent() {
        assert_eq!(
            resolve_level(CrossfaderBus::A, 1.0, 1.0, 0.0, 1.0).unwrap(),
            1.0
        );
        assert_eq!(
            resolve_level(CrossfaderBus::B, 1.0, 1.0, 0.0, 1.0).unwrap(),
            0.0
        );
        assert_eq!(
            resolve_level(CrossfaderBus::A, 1.0, 1.0, 1.0, 1.0).unwrap(),
            0.0
        );
        assert_eq!(
            resolve_level(CrossfaderBus::B, 1.0, 1.0, 1.0, 1.0).unwrap(),
            1.0
        );
    }

    #[test]
    fn bypass_ignores_position_and_folds_trim_mute_master() {
        assert_eq!(
            resolve_level(CrossfaderBus::Bypass, 0.5, 1.0, 0.3, 0.8).unwrap(),
            0.5 * 0.8
        );
        assert_eq!(
            resolve_level(CrossfaderBus::Bypass, 1.0, 0.0, 0.3, 1.0).unwrap(),
            0.0
        );
    }

    #[test]
    fn invalid_position_is_rejected() {
        assert!(resolve_level(CrossfaderBus::A, 1.0, 1.0, 1.5, 1.0).is_err());
    }
}
