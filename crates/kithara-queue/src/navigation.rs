use std::collections::VecDeque;

use kithara_events::TrackId;
use rand::{SeedableRng, rngs::StdRng, seq::SliceRandom};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Deserialize)]
#[non_exhaustive]
pub enum PlaybackOrder {
    #[default]
    Sequential,
    Shuffle,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Deserialize)]
#[non_exhaustive]
pub enum ActionAtItemEnd {
    #[default]
    Advance,
    Pause,
    None,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum RepeatMode {
    #[default]
    Off,
    One,
    All,
}

#[derive(Debug, fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
pub struct NavigationState {
    #[field(get, copy)]
    current: Option<TrackId>,
    #[field(get, copy)]
    playback_order: PlaybackOrder,
    #[field(get, copy, set = set_repeat)]
    repeat_mode: RepeatMode,
    rng: StdRng,
    bag: Vec<TrackId>,
    history: VecDeque<TrackId>,
    #[field(get, copy)]
    history_limit: usize,
}

impl NavigationState {
    #[must_use]
    pub fn new(history_limit: usize) -> Self {
        Self::with_rng(history_limit, StdRng::from_rng(&mut rand::rng()))
    }

    fn fresh_cycle(&mut self, tracks: &[TrackId], avoid_first: Option<TrackId>) {
        self.bag.clear();
        self.bag.extend_from_slice(tracks);
        self.bag.shuffle(&mut self.rng);
        if self.bag.len() > 1
            && let Some(avoid) = avoid_first
            && self.bag.last() == Some(&avoid)
        {
            let last = self.bag.len() - 1;
            self.bag.swap(0, last);
        }
    }

    pub(crate) fn insert(&mut self, id: TrackId) {
        if self.playback_order == PlaybackOrder::Shuffle
            && self.current != Some(id)
            && !self.bag.contains(&id)
        {
            self.bag.push(id);
            self.bag.shuffle(&mut self.rng);
        }
    }

    /// Choose the successor without moving the cursor onto it. A caller reads
    /// this to decide, and the decision can still fall through — the successor
    /// may not be loaded, or the player may refuse it. Moving the cursor here
    /// would strand it on a track that never started while the player stayed
    /// where it was, and the next end-of-item arrives naming a track the cursor
    /// has already left. [`Self::select`] is the commit.
    pub(crate) fn next(
        &mut self,
        tracks: &[TrackId],
        allow_repeat_one: bool,
        allow_wrap: bool,
    ) -> Option<TrackId> {
        if allow_repeat_one && self.repeat_mode == RepeatMode::One {
            return self.current.filter(|id| tracks.contains(id));
        }
        match self.playback_order {
            PlaybackOrder::Sequential => self.next_sequential(tracks, allow_wrap),
            PlaybackOrder::Shuffle => self.next_shuffle(tracks, allow_wrap),
        }
    }

    fn next_sequential(&self, tracks: &[TrackId], allow_wrap: bool) -> Option<TrackId> {
        let Some(current) = self.current else {
            return tracks.first().copied();
        };
        let index = tracks.iter().position(|id| *id == current)?;
        tracks
            .get(index + 1)
            .copied()
            .or_else(|| allow_wrap.then(|| tracks.first().copied()).flatten())
    }

    /// Reads the bag without taking from it: refilling an exhausted bag is part
    /// of choosing, and the chosen item leaves it in [`Self::select`], which
    /// retains it out.
    fn next_shuffle(&mut self, tracks: &[TrackId], allow_wrap: bool) -> Option<TrackId> {
        self.bag.retain(|id| tracks.contains(id));
        if self.bag.is_empty() {
            if !allow_wrap && self.current.is_some() {
                return None;
            }
            self.fresh_cycle(tracks, self.current);
        }
        self.bag.last().copied()
    }

    pub(crate) fn prev(&mut self, tracks: &[TrackId]) -> Option<TrackId> {
        while let Some(previous) = self.history.pop_back() {
            if tracks.contains(&previous) {
                self.current = Some(previous);
                self.bag.retain(|candidate| *candidate != previous);
                return Some(previous);
            }
        }
        None
    }

    fn push_history(&mut self, id: TrackId) {
        if self.history.back() == Some(&id) {
            return;
        }
        if self.history.len() >= self.history_limit {
            self.history.pop_front();
        }
        self.history.push_back(id);
    }

    pub(crate) fn reconcile(&mut self, tracks: &[TrackId]) {
        self.history.retain(|id| tracks.contains(id));
        self.bag.retain(|id| tracks.contains(id));
        if self.current.is_some_and(|id| !tracks.contains(&id)) {
            self.current = None;
        }
    }

    pub(crate) fn select(&mut self, id: TrackId, tracks: &[TrackId]) {
        if let Some(current) = self.current
            && current != id
        {
            self.push_history(current);
        }
        self.current = Some(id);
        if self.playback_order == PlaybackOrder::Shuffle {
            if self.bag.is_empty() {
                self.fresh_cycle(tracks, Some(id));
            }
            self.bag.retain(|candidate| *candidate != id);
        }
    }

    pub(crate) fn set_playback_order(&mut self, order: PlaybackOrder, tracks: &[TrackId]) {
        if self.playback_order == order {
            return;
        }
        self.playback_order = order;
        self.history.clear();
        self.fresh_cycle(tracks, self.current);
    }

    fn with_rng(history_limit: usize, rng: StdRng) -> Self {
        Self {
            history_limit,
            rng,
            current: None,
            repeat_mode: RepeatMode::Off,
            history: VecDeque::new(),
            bag: Vec::new(),
            playback_order: PlaybackOrder::Sequential,
        }
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;

    fn ids() -> [TrackId; 4] {
        [TrackId(1), TrackId(2), TrackId(3), TrackId(4)]
    }

    fn nav() -> NavigationState {
        NavigationState::with_rng(16, StdRng::seed_from_u64(7))
    }

    #[kithara::test]
    fn sequential_repeat_and_history_are_typed() {
        let tracks = ids();
        let mut nav = nav();
        assert_eq!(nav.next(&tracks, true, false), Some(tracks[0]));
        nav.select(tracks[0], &tracks);
        assert_eq!(nav.next(&tracks, true, false), Some(tracks[1]));
        nav.select(tracks[1], &tracks);
        assert_eq!(nav.prev(&tracks), Some(tracks[0]));
        nav.set_repeat(RepeatMode::One);
        assert_eq!(nav.next(&tracks, true, false), Some(tracks[0]));
        assert_eq!(nav.next(&tracks, false, false), Some(tracks[1]));
    }

    #[kithara::test]
    fn shuffle_has_no_cycle_or_boundary_duplicates() {
        let tracks = ids();
        let mut nav = nav();
        nav.set_playback_order(PlaybackOrder::Shuffle, &tracks);
        let cycle = |nav: &mut NavigationState| {
            (0..tracks.len())
                .map(|_| {
                    let id = nav.next(&tracks, false, true).expect("cycle item");
                    nav.select(id, &tracks);
                    id
                })
                .collect::<Vec<_>>()
        };
        let first = cycle(&mut nav);
        let second = cycle(&mut nav);
        let mut first_unique = first.clone();
        first_unique.sort_by_key(|id| id.as_u64());
        first_unique.dedup();
        assert_eq!(first_unique.len(), tracks.len());
        assert_ne!(first.last(), second.first());
        assert_ne!(first, second, "RNG state must advance between cycles");
    }

    #[kithara::test]
    fn explicit_selection_and_removal_reconcile_shuffle() {
        let tracks = ids();
        let mut nav = nav();
        nav.set_playback_order(PlaybackOrder::Shuffle, &tracks);
        nav.select(tracks[2], &tracks);
        assert!(!nav.bag.contains(&tracks[2]));
        let remaining = [tracks[0], tracks[2], tracks[3]];
        nav.reconcile(&remaining);
        assert!(!nav.bag.contains(&tracks[1]));
    }

    #[kithara::test]
    fn shuffle_handles_empty_and_single_item_cycles() {
        let mut nav = nav();
        nav.set_playback_order(PlaybackOrder::Shuffle, &[]);
        assert_eq!(nav.next(&[], false, true), None);

        let only = [TrackId(9)];
        assert_eq!(nav.next(&only, false, true), Some(only[0]));
        nav.select(only[0], &only);
        assert_eq!(nav.next(&only, false, false), None);
        assert_eq!(nav.next(&only, false, true), Some(only[0]));
    }

    #[kithara::test]
    fn previous_uses_real_history_and_never_invents_an_item() {
        let tracks = ids();
        let mut nav = nav();
        nav.set_playback_order(PlaybackOrder::Shuffle, &tracks);
        let first = nav.next(&tracks, false, true).expect("first item");
        nav.select(first, &tracks);
        let second = nav.next(&tracks, false, true).expect("second item");
        nav.select(second, &tracks);
        assert_ne!(first, second);
        assert_eq!(nav.prev(&tracks), Some(first));
        assert_eq!(nav.prev(&tracks), None);
    }

    #[kithara::test]
    fn insertion_joins_the_active_shuffle_cycle() {
        let tracks = ids();
        let mut nav = nav();
        nav.set_playback_order(PlaybackOrder::Shuffle, &tracks[..2]);
        let first = nav.next(&tracks[..2], false, true).expect("cycle item");
        nav.select(first, &tracks[..2]);
        nav.insert(tracks[2]);
        assert!(nav.bag.contains(&tracks[2]));
    }

    /// Reading the successor is not choosing it: the caller can still refuse,
    /// and the cursor must stay on the track that is actually sounding until
    /// `select` commits. Otherwise the next end-of-item names a track the
    /// cursor has already left, and the advance is dropped in silence.
    #[kithara::test]
    fn a_successor_the_caller_never_commits_leaves_the_cursor_where_it_was() {
        let tracks = ids();
        for order in [PlaybackOrder::Sequential, PlaybackOrder::Shuffle] {
            let mut nav = nav();
            nav.set_playback_order(order, &tracks);
            nav.select(tracks[0], &tracks);

            let chosen = nav.next(&tracks, false, true).expect("a successor");
            assert_eq!(
                nav.next(&tracks, false, true),
                Some(chosen),
                "{order:?}: a second read must offer the same successor"
            );
            assert_eq!(
                nav.current(),
                Some(tracks[0]),
                "{order:?}: reading a successor must not move the cursor"
            );

            nav.select(chosen, &tracks);
            assert_eq!(nav.current(), Some(chosen), "{order:?}: select commits");
        }
    }
}
