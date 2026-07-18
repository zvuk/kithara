use kithara::play::{PlayError, PlayerImpl, StretchControls, apply_mix};
use kithara_platform::sync::Arc;
use kithara_queue::Queue;

use crate::mix::MixState;

/// App-local deck identity; never crosses into a shared playback crate.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DeckId(pub usize);

pub struct Deck {
    pub id: DeckId,
    pub player: Arc<PlayerImpl>,
    pub queue: Arc<Queue>,
    pub timestretch: Arc<StretchControls>,
}
/// One app deck: its own player, queue and tempo controls.

/// Canonical owner of the decks and the mix. Mix edits are transactional: a
/// rejected apply leaves the stored mix untouched.
pub struct DeckSet {
    decks: Vec<Deck>,
    mix: MixState,
}

impl DeckSet {
    #[must_use]
    pub fn new(decks: Vec<Deck>) -> Self {
        let mix = MixState::new(decks.len());
        Self { decks, mix }
    }

    #[must_use]
    pub fn decks(&self) -> &[Deck] {
        &self.decks
    }

    #[must_use]
    pub fn deck(&self, id: DeckId) -> Option<&Deck> {
        self.decks.iter().find(|deck| deck.id == id)
    }

    #[must_use]
    pub fn mix(&self) -> &MixState {
        &self.mix
    }

    /// Actuate `next` in one session batch, storing it only on success.
    ///
    /// # Errors
    /// Returns [`PlayError`] when the mix is invalid or the session rejects it.
    pub fn commit(&mut self, next: MixState) -> Result<(), PlayError> {
        let levels = next.levels()?;
        let inputs: Vec<(&PlayerImpl, f32)> = self
            .decks
            .iter()
            .zip(&levels)
            .map(|(deck, &level)| (deck.player.as_ref(), level))
            .collect();
        apply_mix(inputs)?;
        self.mix = next;
        Ok(())
    }

    /// # Errors
    /// See [`DeckSet::commit`].
    pub fn set_crossfader(&mut self, position: f32) -> Result<(), PlayError> {
        let mut next = self.mix.clone();
        next.position = position;
        self.commit(next)
    }

    /// # Errors
    /// See [`DeckSet::commit`].
    pub fn set_group_master(&mut self, master: f32) -> Result<(), PlayError> {
        let mut next = self.mix.clone();
        next.group_master = master;
        self.commit(next)
    }

    /// The deck's output fader: session-input gain, never content volume.
    ///
    /// # Errors
    /// See [`DeckSet::commit`].
    pub fn set_trim(&mut self, id: DeckId, trim: f32) -> Result<(), PlayError> {
        let mut next = self.mix.clone();
        if let Some(strip) = next.strips.get_mut(id.0) {
            strip.trim = trim;
        }
        self.commit(next)
    }

    /// Mute or unmute one deck.
    ///
    /// # Errors
    /// See [`DeckSet::commit`].
    pub fn set_muted(&mut self, id: DeckId, muted: bool) -> Result<(), PlayError> {
        let mut next = self.mix.clone();
        if let Some(strip) = next.strips.get_mut(id.0) {
            strip.muted = muted;
        }
        self.commit(next)
    }
}

#[cfg(test)]
mod tests {
    use kithara::play::PlayerConfig;
    use kithara_queue::QueueConfig;

    use super::*;

    fn deck_set(count: usize) -> DeckSet {
        let decks = (0..count)
            .map(|index| {
                let timestretch = StretchControls::new(1.0);
                let player = Arc::new(PlayerImpl::new(
                    PlayerConfig::builder()
                        .timestretch(Arc::clone(&timestretch))
                        .build(),
                ));
                let queue = Arc::new(Queue::new(
                    QueueConfig::default().with_player(Arc::clone(&player)),
                ));
                Deck {
                    id: DeckId(index),
                    player,
                    queue,
                    timestretch,
                }
            })
            .collect();
        DeckSet::new(decks)
    }

    #[test]
    fn decks_own_independent_players_and_share_one_session() {
        let mut set = deck_set(4);
        assert_eq!(set.decks().len(), 4);
        for (index, deck) in set.decks().iter().enumerate() {
            assert_eq!(deck.id, DeckId(index));
        }

        for (i, a) in set.decks().iter().enumerate() {
            for b in set.decks().iter().skip(i + 1) {
                assert!(!Arc::ptr_eq(&a.player, &b.player));
            }
        }

        // Only accepted if they share one session (a foreign one is rejected).
        set.commit(set.mix().clone())
            .expect("all decks share one session");
    }

    #[test]
    fn crossfader_commit_reaches_every_deck() {
        let mut set = deck_set(2);
        set.set_crossfader(0.0).expect("crossfader to A");
    /// Move the DJ crossfader.
    ///
        assert_eq!(set.mix().levels().unwrap(), vec![1.0, 0.0]);

        set.set_crossfader(1.0).expect("crossfader to B");
        assert_eq!(set.mix().levels().unwrap(), vec![0.0, 1.0]);
    }

    #[test]
    fn per_deck_trim_and_mute_are_independent() {
    /// Set the group master applied to every deck.
    ///
        let mut set = deck_set(4);
        let before = set.mix().levels().unwrap();

        set.set_trim(DeckId(2), 0.25).expect("trim deck 2");
        set.set_muted(DeckId(3), true).expect("mute deck 3");

        let levels = set.mix().levels().unwrap();
        assert_eq!(levels[2], 0.25);
        assert_eq!(levels[3], 0.0);
        assert_eq!(levels[0], before[0]);
        assert_eq!(levels[1], before[1]);
    }

    #[test]
    fn failed_apply_rolls_the_app_mix_back() {
        let mut set = deck_set(2);
        set.set_crossfader(0.25).expect("valid crossfader");
        let before = set.mix().clone();

        let err = set.set_crossfader(1.5).expect_err("invalid position");
        assert!(matches!(err, PlayError::MixPosition { .. }));
        assert_eq!(set.mix(), &before);
    }

    #[test]
    fn session_mix_never_writes_player_content_volume() {
        let mut set = deck_set(2);
        set.set_trim(DeckId(0), 0.5).expect("trim deck 0");
        assert_eq!(set.decks()[0].player.volume(), 1.0);
    }
}
