use kithara::host::{CrossfaderBus, PlayError, crossfader_gain};

/// One deck's channel strip.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MixStrip {
    pub bus: CrossfaderBus,
    pub muted: bool,
    pub trim: f32,
}

impl MixStrip {
    /// Deck 0 -> A, deck 1 -> B, anything further (or a lone deck) -> `Bypass`.
    #[must_use]
    pub const fn new(index: usize, count: usize) -> Self {
        let bus = match (count >= 2, index) {
            (true, 0) => CrossfaderBus::A,
            (true, 1) => CrossfaderBus::B,
            _ => CrossfaderBus::Bypass,
        };

        Self {
            bus,
            trim: 1.0,
            muted: false,
        }
    }
}

/// The global mix values; `DeckSet` owns them.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MixState {
    pub group_master: f32,
    pub position: f32,
}

impl Default for MixState {
    fn default() -> Self {
        Self {
            position: 0.5,
            group_master: 1.0,
        }
    }
}

impl MixState {
    /// One strip's level, as [`MixState::levels`] computes it.
    ///
    /// # Errors
    /// See [`MixState::levels`].
    pub(crate) fn level(self, strip: MixStrip) -> Result<f32, PlayError> {
        let gain = crossfader_gain(strip.bus, self.position)?;
        let mute = if strip.muted { 0.0 } else { 1.0 };
        let level = (strip.trim * mute * gain * self.group_master).clamp(0.0, 1.0);

        if level.is_nan() {
            return Err(PlayError::MixLevel { level });
        }

        Ok(level)
    }

    /// `trim * mute * crossfader_gain(bus, position) * group_master` per strip.
    ///
    /// # Errors
    /// Returns [`PlayError::MixPosition`] for a position outside `0.0..=1.0` and
    /// [`PlayError::MixLevel`] for a level that is not a number.
    pub fn levels<'a, I>(&self, strips: I) -> Result<Vec<f32>, PlayError>
    where
        I: IntoIterator<Item = &'a MixStrip>,
    {
        strips.into_iter().map(|strip| self.level(*strip)).collect()
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;

    fn two() -> [MixStrip; 2] {
        [MixStrip::new(0, 2), MixStrip::new(1, 2)]
    }

    #[kithara::test(native, flash(false))]
    fn two_decks_are_assigned_to_the_a_and_b_buses() {
        let strips = two();
        assert_eq!(strips[0].bus, CrossfaderBus::A);
        assert_eq!(strips[1].bus, CrossfaderBus::B);
    }

    #[kithara::test(native, flash(false))]
    fn a_single_deck_bypasses_the_crossfader() {
        let strip = MixStrip::new(0, 1);
        assert_eq!(strip.bus, CrossfaderBus::Bypass);
        assert_eq!(MixState::default().levels([&strip]).unwrap(), vec![1.0]);
    }

    #[kithara::test(native, flash(false))]
    fn crossfader_endpoints_resolve_to_full_and_silent() {
        let strips = two();
        let mut mix = MixState {
            position: 0.0,
            ..MixState::default()
        };

        assert_eq!(mix.levels(&strips).unwrap(), vec![1.0, 0.0]);

        mix.position = 1.0;
        assert_eq!(mix.levels(&strips).unwrap(), vec![0.0, 1.0]);
    }

    #[kithara::test(native, flash(false))]
    fn trim_mute_and_group_master_fold_into_the_level() {
        let mut strips = two();
        let mix = MixState {
            position: 0.0,
            group_master: 0.5,
        };

        strips[0].trim = 0.5;
        strips[1].muted = true;

        // Deck 0: trim 0.5 * A-gain 1.0 * master 0.5. Deck 1: muted.
        assert_eq!(mix.levels(&strips).unwrap(), vec![0.25, 0.0]);
    }

    #[kithara::test(native)]
    #[case::trim(|_: &mut MixState, strips: &mut [MixStrip; 2]| strips[1].trim = f32::NAN)]
    #[case::group_master(|mix: &mut MixState, _: &mut [MixStrip; 2]| mix.group_master = f32::NAN)]
    fn a_level_that_is_not_a_number_is_refused_before_any_deck_takes_one(
        #[case] spoil: fn(&mut MixState, &mut [MixStrip; 2]),
    ) {
        let mut strips = two();
        let mut mix = MixState {
            position: 0.0,
            ..MixState::default()
        };

        spoil(&mut mix, &mut strips);

        assert!(matches!(
            mix.levels(&strips),
            Err(PlayError::MixLevel { .. })
        ));
    }

    #[kithara::test(native, flash(false))]
    fn an_invalid_position_is_rejected_rather_than_clamped() {
        let mix = MixState {
            position: 1.5,
            ..MixState::default()
        };

        assert!(matches!(
            mix.levels(&two()),
            Err(PlayError::MixPosition { .. })
        ));
    }
}
