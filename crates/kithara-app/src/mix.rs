use kithara::play::{CrossfaderBus, PlayError, crossfader_gain};

/// One deck's channel strip.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MixStrip {
    pub bus: CrossfaderBus,
    pub trim: f32,
    pub muted: bool,
}

impl MixStrip {
    #[must_use]
    pub fn new(bus: CrossfaderBus) -> Self {
        Self {
            bus,
            trim: 1.0,
            muted: false,
        }
    }
}

/// The app's desired mix. A plain value; `DeckSet` is its single owner.
#[derive(Clone, Debug, PartialEq)]
pub struct MixState {
    pub position: f32,
    pub group_master: f32,
    pub strips: Vec<MixStrip>,
}

impl MixState {
    /// Deck 0 -> A, deck 1 -> B, anything further (or a lone deck) -> `Bypass`.
    #[must_use]
    pub fn new(count: usize) -> Self {
        let strips = (0..count)
            .map(|i| MixStrip::new(bus_for(i, count)))
            .collect();
        Self {
            position: 0.5,
            group_master: 1.0,
            strips,
        }
    }

    /// `trim * mute * crossfader_gain(bus, position) * group_master` per deck.
    ///
    /// # Errors
    /// Returns [`PlayError::MixPosition`] when the crossfader position is not a
    /// finite value in `0.0..=1.0`.
    pub fn levels(&self) -> Result<Vec<f32>, PlayError> {
        self.strips
            .iter()
            .map(|strip| {
                let gain = crossfader_gain(strip.bus, self.position)?;
                let mute = if strip.muted { 0.0 } else { 1.0 };
                Ok((strip.trim * mute * gain * self.group_master).clamp(0.0, 1.0))
            })
            .collect()
    }
}

fn bus_for(index: usize, count: usize) -> CrossfaderBus {
    match (count >= 2, index) {
        (true, 0) => CrossfaderBus::A,
        (true, 1) => CrossfaderBus::B,
        _ => CrossfaderBus::Bypass,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_decks_are_assigned_to_the_a_and_b_buses() {
        let mix = MixState::new(2);
        assert_eq!(mix.strips[0].bus, CrossfaderBus::A);
        assert_eq!(mix.strips[1].bus, CrossfaderBus::B);
    }

    #[test]
    fn a_single_deck_bypasses_the_crossfader() {
        let mix = MixState::new(1);
        assert_eq!(mix.strips[0].bus, CrossfaderBus::Bypass);
        assert_eq!(mix.levels().unwrap(), vec![1.0]);
    }

    #[test]
    fn crossfader_endpoints_resolve_to_full_and_silent() {
        let mut mix = MixState::new(2);

        mix.position = 0.0;
        assert_eq!(mix.levels().unwrap(), vec![1.0, 0.0]);

        mix.position = 1.0;
        assert_eq!(mix.levels().unwrap(), vec![0.0, 1.0]);
    }

    #[test]
    fn trim_mute_and_group_master_fold_into_the_level() {
        let mut mix = MixState::new(2);
        mix.position = 0.0;
        mix.group_master = 0.5;
        mix.strips[0].trim = 0.5;
        mix.strips[1].muted = true;

        // Deck 0: trim 0.5 * A-gain 1.0 * master 0.5. Deck 1: muted.
        assert_eq!(mix.levels().unwrap(), vec![0.25, 0.0]);
    }

    #[test]
    fn an_invalid_position_is_rejected_rather_than_clamped() {
        let mut mix = MixState::new(2);
        mix.position = 1.5;
        assert!(matches!(mix.levels(), Err(PlayError::MixPosition { .. })));
    }
}
