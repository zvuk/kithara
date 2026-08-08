use kithara::{
    audio::{EqBandConfig, generate_log_spaced_bands},
    play::{PlayError, PlayerConfig, PlayerImpl, SessionHandle, StretchControls, apply_mix},
};
use kithara_platform::sync::Arc;
use kithara_queue::{Queue, QueueConfig};

use crate::{config::AppConfig, mix::MixState};

/// EQ topology shared by every studio deck and its player graph.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum EqMode {
    #[default]
    ThreeBand,
    FourBand,
}

impl EqMode {
    const FOUR_BANDS: [&str; 4] = ["low", "low_mid", "high_mid", "high"];
    const THREE_BANDS: [&str; 3] = ["low", "mid", "high"];

    #[must_use]
    pub(crate) fn band(self, name: &str) -> Option<usize> {
        self.bands().iter().position(|band| *band == name)
    }

    #[must_use]
    pub(crate) const fn bands(self) -> &'static [&'static str] {
        match self {
            Self::ThreeBand => &Self::THREE_BANDS,
            Self::FourBand => &Self::FOUR_BANDS,
        }
    }

    #[must_use]
    pub(crate) fn layout(self, gains: &[f32]) -> Vec<EqBandConfig> {
        let mut layout = generate_log_spaced_bands(self.bands().len());
        for (band, gain) in layout.iter_mut().zip(gains) {
            band.set_gain_db(*gain);
        }
        layout
    }

    #[must_use]
    pub(crate) fn remap(self, next: Self, gains: &[f32]) -> Option<Vec<f32>> {
        match (self, next, gains) {
            (Self::ThreeBand, Self::FourBand, [low, mid, high]) => {
                Some(vec![*low, *mid, *mid, *high])
            }
            (Self::FourBand, Self::ThreeBand, [low, low_mid, high_mid, high]) => {
                Some(vec![*low, (*low_mid + *high_mid) * 0.5, *high])
            }
            _ => None,
        }
    }
}

/// App-local deck identity; never crosses into a shared playback crate.
///
/// Stable for the deck's lifetime: decks come and go at runtime, so an id is
/// never a position in any list.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DeckId(pub usize);

/// One app deck: its own player, queue and tempo controls.
pub struct Deck {
    pub player: Arc<PlayerImpl>,
    pub queue: Arc<Queue>,
    pub timestretch: Arc<StretchControls>,
    pub id: DeckId,
}

impl Deck {
    /// Build a deck with its own player, queue and time-stretch handle, all
    /// hanging off the app's shutdown token. Every deck joins `session`: the
    /// mix batch only accepts players of one shared audio session.
    #[must_use]
    pub fn build(id: DeckId, config: &AppConfig, session: &SessionHandle) -> Self {
        let timestretch = StretchControls::new(1.0);
        let player = Arc::new(PlayerImpl::new(
            PlayerConfig::builder()
                .cancel(config.shutdown.child())
                .crossfade_duration(config.crossfade_seconds)
                .eq_layout(generate_log_spaced_bands(EqMode::default().bands().len()))
                .byte_pool(config.byte_pool.clone())
                .pcm_pool(config.pcm_pool.clone())
                .session(session.dispatcher())
                .timestretch(Arc::clone(&timestretch))
                .build(),
        ));
        let queue = Arc::new(Queue::new(
            QueueConfig::builder()
                .player(Arc::clone(&player))
                .store(config.store.clone())
                .cancel(config.shutdown.child())
                .build(),
        ));

        Self {
            player,
            queue,
            timestretch,
            id,
        }
    }
}

/// Canonical owner of the decks and the mix. Mix edits are transactional: a
/// rejected apply leaves the stored mix untouched.
///
/// The deck list is dynamic. `mix.strips` is kept parallel to `decks` — same
/// length, same order — by this type alone; every lookup goes through
/// [`DeckSet::position`], never through a raw [`DeckId`] value.
pub struct DeckSet {
    mix: MixState,
    decks: Vec<Deck>,
    next_id: usize,
}

impl DeckSet {
    #[must_use]
    pub fn new(decks: Vec<Deck>) -> Self {
        let next_id = decks.iter().map(|deck| deck.id.0 + 1).max().unwrap_or(0);
        let mix = MixState::new(decks.len());
        Self {
            mix,
            decks,
            next_id,
        }
    }

    /// Add a deck and re-derive the crossfader buses around it, keeping the
    /// levels of the decks that were already playing.
    ///
    /// # Errors
    /// See [`DeckSet::commit`]. A rejected apply leaves the set unchanged.
    pub fn add(&mut self, deck: Deck) -> Result<(), PlayError> {
        self.next_id = self.next_id.max(deck.id.0 + 1);
        self.decks.push(deck);
        let next = self.mix.resized(self.decks.len());
        if let Err(e) = self.commit(next) {
            self.decks.pop();
            return Err(e);
        }
        Ok(())
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

    #[must_use]
    pub fn deck(&self, id: DeckId) -> Option<&Deck> {
        self.decks.iter().find(|deck| deck.id == id)
    }

    #[must_use]
    pub fn decks(&self) -> &[Deck] {
        &self.decks
    }

    #[must_use]
    pub fn mix(&self) -> &MixState {
        &self.mix
    }

    /// Id for the next deck; never reuses one that has been handed out.
    pub fn next_id(&mut self) -> DeckId {
        let id = DeckId(self.next_id);
        self.next_id += 1;
        id
    }

    /// Where `id` currently sits in the deck list (and so in the mix).
    #[must_use]
    pub fn position(&self, id: DeckId) -> Option<usize> {
        self.decks.iter().position(|deck| deck.id == id)
    }

    /// Remove a deck, silencing it before it goes: its player leaves the
    /// session with the same batch that re-derives the remaining buses.
    ///
    /// # Errors
    /// See [`DeckSet::commit`]. A rejected apply leaves the set unchanged.
    pub fn remove(&mut self, id: DeckId) -> Result<(), PlayError> {
        let Some(index) = self.position(id) else {
            return Ok(());
        };
        let deck = self.decks.remove(index);
        let mut next = self.mix.clone();
        next.strips.remove(index);
        let next = next.resized(self.decks.len());
        if let Err(e) = self.commit(next) {
            self.decks.insert(index, deck);
            return Err(e);
        }
        Ok(())
    }

    /// Move the DJ crossfader.
    ///
    /// # Errors
    /// See [`DeckSet::commit`].
    pub fn set_crossfader(&mut self, position: f32) -> Result<(), PlayError> {
        let mut next = self.mix.clone();
        next.position = position;
        self.commit(next)
    }

    /// Mute or unmute one deck.
    ///
    /// # Errors
    /// See [`DeckSet::commit`].
    pub fn set_muted(&mut self, id: DeckId, muted: bool) -> Result<(), PlayError> {
        let mut next = self.mix.clone();
        if let Some(strip) = self.position(id).and_then(|at| next.strips.get_mut(at)) {
            strip.muted = muted;
        }
        self.commit(next)
    }

    /// The deck's output fader: session-input gain, never content volume.
    ///
    /// # Errors
    /// See [`DeckSet::commit`].
    pub fn set_trim(&mut self, id: DeckId, trim: f32) -> Result<(), PlayError> {
        let mut next = self.mix.clone();
        if let Some(strip) = self.position(id).and_then(|at| next.strips.get_mut(at)) {
            strip.trim = trim;
        }
        self.commit(next)
    }
}

#[cfg(test)]
mod tests {
    use kithara::{
        bufpool::{BytePool, PcmPool},
        play::PlayerConfig,
    };
    use kithara_queue::QueueConfig;

    use super::*;

    fn one_deck(id: DeckId, session: &SessionHandle) -> Deck {
        let timestretch = StretchControls::new(1.0);
        let player = Arc::new(PlayerImpl::new(
            PlayerConfig::builder()
                .byte_pool(BytePool::default())
                .pcm_pool(PcmPool::default())
                .session(session.dispatcher())
                .timestretch(Arc::clone(&timestretch))
                .build(),
        ));
        let queue = Arc::new(Queue::new(
            QueueConfig::builder().player(Arc::clone(&player)).build(),
        ));
        Deck {
            player,
            queue,
            timestretch,
            id,
        }
    }

    fn deck_set_on(count: usize, session: &SessionHandle) -> DeckSet {
        DeckSet::new(
            (0..count)
                .map(|index| one_deck(DeckId(index), session))
                .collect(),
        )
    }

    fn deck_set(count: usize) -> DeckSet {
        deck_set_on(count, &SessionHandle::spawn_native())
    }

    #[test]
    fn eq_mode_maps_the_middle_band_without_moving_the_outer_bands() {
        let four = EqMode::ThreeBand
            .remap(EqMode::FourBand, &[-6.0, 2.0, 5.0])
            .unwrap();
        assert_eq!(four, [-6.0, 2.0, 2.0, 5.0]);

        let three = EqMode::FourBand
            .remap(EqMode::ThreeBand, &[-6.0, -2.0, 4.0, 5.0])
            .unwrap();
        assert_eq!(three, [-6.0, 1.0, 5.0]);
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
        assert_eq!(set.mix().levels().unwrap(), vec![1.0, 0.0]);

        set.set_crossfader(1.0).expect("crossfader to B");
        assert_eq!(set.mix().levels().unwrap(), vec![0.0, 1.0]);
    }

    #[test]
    fn per_deck_trim_and_mute_are_independent() {
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
    fn adding_a_second_deck_turns_the_crossfader_on() {
        let session = SessionHandle::spawn_native();
        let mut set = deck_set_on(1, &session);
        assert_eq!(set.mix().levels().unwrap(), vec![1.0], "lone deck bypasses");

        let id = set.next_id();
        set.add(one_deck(id, &session)).expect("add a deck");
        set.set_crossfader(0.0).expect("crossfader to A");

        assert_eq!(set.decks().len(), 2);
        assert_eq!(set.mix().levels().unwrap(), vec![1.0, 0.0]);
    }

    #[test]
    fn removing_a_deck_keeps_the_survivors_settings() {
        let mut set = deck_set(3);
        set.set_trim(DeckId(2), 0.25).expect("trim deck 2");

        set.remove(DeckId(0)).expect("remove deck 0");

        assert_eq!(set.decks().len(), 2);
        assert_eq!(set.decks()[0].id, DeckId(1), "ids stay stable");
        let levels = set.mix().levels().unwrap();
        assert_eq!(levels.len(), 2);
        // Deck 2 kept its trim and moved onto the B bus (position 1).
        assert_eq!(set.mix().strips[1].trim, 0.25);
    }

    #[test]
    fn a_removed_deck_leaves_no_id_reuse() {
        let mut set = deck_set(2);
        set.remove(DeckId(1)).expect("remove deck 1");
        assert_eq!(set.next_id(), DeckId(2));
    }

    #[test]
    fn session_mix_never_writes_player_content_volume() {
        let mut set = deck_set(2);
        set.set_trim(DeckId(0), 0.5).expect("trim deck 0");
        assert_eq!(set.decks()[0].player.volume(), 1.0);
    }
}
