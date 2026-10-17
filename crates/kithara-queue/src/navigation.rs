use std::collections::VecDeque;

use kithara_events::TrackId;
use rand::{SeedableRng, rngs::StdRng, seq::SliceRandom};

use crate::AdvanceReason;

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

    pub(crate) fn insert(&mut self, id: TrackId, current: Option<TrackId>) {
        if self.playback_order == PlaybackOrder::Shuffle
            && current != Some(id)
            && !self.bag.contains(&id)
        {
            self.bag.push(id);
            self.bag.shuffle(&mut self.rng);
        }
    }

    pub(crate) fn next(
        &mut self,
        tracks: &[TrackId],
        current: Option<TrackId>,
        allow_repeat_one: bool,
        allow_wrap: bool,
    ) -> Option<TrackId> {
        if allow_repeat_one && self.repeat_mode == RepeatMode::One {
            return current.filter(|id| tracks.contains(id));
        }
        match self.playback_order {
            PlaybackOrder::Sequential => Self::next_sequential(tracks, current, allow_wrap),
            PlaybackOrder::Shuffle => self.next_shuffle(tracks, current, allow_wrap),
        }
    }

    pub(crate) fn next_sequential(
        tracks: &[TrackId],
        current: Option<TrackId>,
        allow_wrap: bool,
    ) -> Option<TrackId> {
        let Some(current) = current else {
            return tracks.first().copied();
        };
        let index = tracks.iter().position(|id| *id == current)?;
        tracks
            .get(index + 1)
            .copied()
            .or_else(|| allow_wrap.then(|| tracks.first().copied()).flatten())
    }

    fn next_shuffle(
        &mut self,
        tracks: &[TrackId],
        current: Option<TrackId>,
        allow_wrap: bool,
    ) -> Option<TrackId> {
        self.bag.retain(|id| tracks.contains(id));
        if self.bag.is_empty() || (allow_wrap && self.bag.iter().all(|id| current == Some(*id))) {
            if !allow_wrap && current.is_some() {
                return None;
            }
            self.fresh_cycle(tracks, current);
        }
        self.bag
            .iter()
            .rev()
            .copied()
            .find(|id| current != Some(*id) || (tracks.len() == 1 && allow_wrap))
    }

    pub(crate) fn prev(&self, tracks: &[TrackId]) -> Option<TrackId> {
        self.history
            .iter()
            .rev()
            .copied()
            .find(|id| tracks.contains(id))
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
    }

    pub(crate) fn commit(
        &mut self,
        from: Option<TrackId>,
        id: TrackId,
        reason: AdvanceReason,
        tracks: &[TrackId],
    ) {
        if reason == AdvanceReason::UserPrev {
            if let Some(index) = self.history.iter().rposition(|previous| *previous == id) {
                self.history.truncate(index);
            }
        } else if let Some(current) = from.filter(|current| *current != id) {
            self.push_history(current);
        }
        if self.playback_order == PlaybackOrder::Shuffle {
            if self.bag.is_empty() {
                self.fresh_cycle(tracks, Some(id));
            }
            self.bag.retain(|candidate| *candidate != id);
        }
    }

    pub(crate) fn set_playback_order(
        &mut self,
        order: PlaybackOrder,
        tracks: &[TrackId],
        current: Option<TrackId>,
    ) {
        if self.playback_order == order {
            return;
        }
        self.playback_order = order;
        self.history.clear();
        self.fresh_cycle(tracks, current);
    }

    fn with_rng(history_limit: usize, rng: StdRng) -> Self {
        Self {
            history_limit,
            rng,
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

    fn select(
        nav: &mut NavigationState,
        current: &mut Option<TrackId>,
        id: TrackId,
        tracks: &[TrackId],
    ) {
        nav.commit(*current, id, AdvanceReason::UserSelect, tracks);
        *current = Some(id);
    }

    fn previous(
        nav: &mut NavigationState,
        current: &mut Option<TrackId>,
        tracks: &[TrackId],
    ) -> Option<TrackId> {
        let id = nav.prev(tracks)?;
        nav.commit(*current, id, AdvanceReason::UserPrev, tracks);
        *current = Some(id);
        Some(id)
    }

    #[kithara::test]
    fn reading_previous_preserves_history_and_shuffle_bag() {
        let tracks = ids();
        for order in [PlaybackOrder::Sequential, PlaybackOrder::Shuffle] {
            let mut current = None;
            let mut nav = nav();
            nav.set_playback_order(order, &tracks, current);
            select(&mut nav, &mut current, tracks[0], &tracks);
            select(&mut nav, &mut current, tracks[1], &tracks);
            select(&mut nav, &mut current, tracks[2], &tracks);
            let history = nav.history.clone();
            let bag = nav.bag.clone();
            assert_eq!(nav.prev(&tracks), Some(tracks[1]));
            assert_eq!(nav.history, history, "a choice is not a committed Previous");
            assert_eq!(nav.bag, bag, "a refused choice cannot spend the bag");
            assert_eq!(nav.prev(&tracks), Some(tracks[1]));
        }
    }

    #[kithara::test]
    fn choosing_next_and_repeat_does_not_spend_history_or_bag() {
        let tracks = ids();
        let mut current = None;
        let mut nav = nav();
        nav.set_playback_order(PlaybackOrder::Shuffle, &tracks, current);
        select(&mut nav, &mut current, tracks[0], &tracks);
        let history = nav.history.clone();
        let bag = nav.bag.clone();
        let chosen = nav
            .next(&tracks, current, false, false)
            .expect("next choice");
        assert_eq!(nav.next(&tracks, current, false, false), Some(chosen));
        assert_eq!(nav.history, history);
        assert_eq!(nav.bag, bag);
        nav.set_repeat(RepeatMode::One);
        assert_eq!(nav.next(&tracks, current, true, false), Some(tracks[0]));
        assert_eq!(nav.history, history);
        assert_eq!(nav.bag, bag);
    }

    #[kithara::test]
    fn sequential_repeat_and_history_are_typed() {
        let tracks = ids();
        let mut current = None;
        let mut nav = nav();
        assert_eq!(nav.next(&tracks, current, true, false), Some(tracks[0]));
        select(&mut nav, &mut current, tracks[0], &tracks);
        assert_eq!(nav.next(&tracks, current, true, false), Some(tracks[1]));
        select(&mut nav, &mut current, tracks[1], &tracks);
        assert_eq!(previous(&mut nav, &mut current, &tracks), Some(tracks[0]));
        nav.set_repeat(RepeatMode::One);
        assert_eq!(nav.next(&tracks, current, true, false), Some(tracks[0]));
        assert_eq!(nav.next(&tracks, current, false, false), Some(tracks[1]));
    }

    #[kithara::test]
    fn shuffle_has_no_cycle_or_boundary_duplicates() {
        let tracks = ids();
        let mut current = None;
        let mut nav = nav();
        nav.set_playback_order(PlaybackOrder::Shuffle, &tracks, current);
        let mut cycle = |nav: &mut NavigationState| {
            (0..tracks.len())
                .map(|_| {
                    let id = nav.next(&tracks, current, false, true).expect("cycle item");
                    select(nav, &mut current, id, &tracks);
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
        let mut current = None;
        let mut nav = nav();
        nav.set_playback_order(PlaybackOrder::Shuffle, &tracks, current);
        select(&mut nav, &mut current, tracks[2], &tracks);
        assert!(!nav.bag.contains(&tracks[2]));
        let remaining = [tracks[0], tracks[2], tracks[3]];
        nav.reconcile(&remaining);
        assert!(!nav.bag.contains(&tracks[1]));
    }

    #[kithara::test]
    fn shuffle_handles_empty_and_single_item_cycles() {
        let mut current = None;
        let mut nav = nav();
        nav.set_playback_order(PlaybackOrder::Shuffle, &[], current);
        assert_eq!(nav.next(&[], current, false, true), None);

        let only = [TrackId(9)];
        assert_eq!(nav.next(&only, current, false, true), Some(only[0]));
        select(&mut nav, &mut current, only[0], &only);
        assert_eq!(nav.next(&only, current, false, false), None);
        assert_eq!(nav.next(&only, current, false, true), Some(only[0]));
    }

    #[kithara::test]
    fn previous_uses_real_history_and_never_invents_an_item() {
        let tracks = ids();
        let mut current = None;
        let mut nav = nav();
        nav.set_playback_order(PlaybackOrder::Shuffle, &tracks, current);
        let first = nav.next(&tracks, current, false, true).expect("first item");
        select(&mut nav, &mut current, first, &tracks);
        let second = nav
            .next(&tracks, current, false, true)
            .expect("second item");
        select(&mut nav, &mut current, second, &tracks);
        assert_ne!(first, second);
        assert_eq!(previous(&mut nav, &mut current, &tracks), Some(first));
        assert_eq!(previous(&mut nav, &mut current, &tracks), None);
    }

    #[kithara::test]
    fn insertion_joins_the_active_shuffle_cycle() {
        let tracks = ids();
        let mut current = None;
        let mut nav = nav();
        nav.set_playback_order(PlaybackOrder::Shuffle, &tracks[..2], current);
        let first = nav
            .next(&tracks[..2], current, false, true)
            .expect("cycle item");
        select(&mut nav, &mut current, first, &tracks[..2]);
        nav.insert(tracks[2], current);
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
            let mut current = None;
            let mut nav = nav();
            nav.set_playback_order(order, &tracks, current);
            select(&mut nav, &mut current, tracks[0], &tracks);

            let chosen = nav
                .next(&tracks, current, false, true)
                .expect("a successor");
            assert_eq!(
                nav.next(&tracks, current, false, true),
                Some(chosen),
                "{order:?}: a second read must offer the same successor"
            );
            assert_eq!(
                current,
                Some(tracks[0]),
                "{order:?}: reading a successor must not move the cursor"
            );

            select(&mut nav, &mut current, chosen, &tracks);
            assert_eq!(current, Some(chosen), "{order:?}: select commits");
        }
    }
}
