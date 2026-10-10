//! The tracks a queue holds on its mixer, staged, and in the background.

use kithara_command::Seq;
use kithara_events::TrackId;
use kithara_play::{DeckSnapshot, Slot};
use kithara_signal::SessionFrame;

/// A track loaded in the background: it holds a decoder lane and no slot.
pub(super) struct Parked<T> {
    pub(super) item: TrackId,
    pub(super) track: T,
    pub(super) load: Option<LoadState>,
}

/// What an active track is to the queue.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Role {
    /// Sounds as the queue's current item.
    Current,
    /// The transition target; `batch` is the deck batch that brings it in,
    /// once sent.
    Incoming { batch: Option<Seq> },
    /// Loaded ahead of the current track's end; not yet a target.
    Preloaded,
    /// Plays its tail out after a transition replaced it.
    Outgoing,
    /// Released; waits for its slot to let it go.
    Leaving,
}

/// One track on the deck's mixer.
pub(super) struct Active<T> {
    pub(super) item: TrackId,
    pub(super) slot: Slot,
    pub(super) track: T,
    pub(super) role: Role,
    pub(super) load: Option<LoadState>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum LoadState {
    Opening(Seq),
    Attaching(Seq),
}

impl LoadState {
    pub(super) fn seq(self) -> Seq {
        match self {
            Self::Opening(seq) | Self::Attaching(seq) => seq,
        }
    }
}

/// Slot owners, staged replacements, and background tracks waiting off the deck.
pub(super) struct Slots<T> {
    capacity: usize,
    parked: Vec<Parked<T>>,
    active: Vec<Active<T>>,
    staged: Vec<Active<T>>,
    fades: Vec<Option<SessionFrame>>,
}

impl<T> Slots<T> {
    pub(super) fn new(capacity: usize) -> Self {
        Self {
            capacity,
            parked: Vec::new(),
            active: Vec::with_capacity(capacity),
            staged: Vec::new(),
            fades: vec![None; capacity],
        }
    }

    /// The lowest slot no active track holds.
    pub(super) fn free_slot(&self) -> Option<Slot> {
        (0..self.capacity)
            .find(|index| {
                self.iter()
                    .all(|active| usize::from(active.slot.get()) != *index)
            })
            .and_then(|index| u16::try_from(index).ok())
            .map(Slot::new)
    }

    /// The track to evict for a new one: the quietest by the mixer's last
    /// gain among those `evictable` allows; a fade nearer its end is quieter.
    pub(super) fn quietest(
        &self,
        deck: &DeckSnapshot,
        evictable: impl Fn(&Active<T>) -> bool,
    ) -> Option<usize> {
        self.active
            .iter()
            .enumerate()
            .filter(|(_, active)| evictable(active))
            .min_by(|(_, first), (_, second)| {
                match (self.fade_end(first.slot), self.fade_end(second.slot)) {
                    (Some(first), Some(second)) => first.cmp(&second),
                    (Some(_), None) => std::cmp::Ordering::Less,
                    (None, Some(_)) => std::cmp::Ordering::Greater,
                    (None, None) => gain(deck, first.slot).total_cmp(&gain(deck, second.slot)),
                }
            })
            .map(|(index, _)| index)
    }

    pub(super) fn push(&mut self, active: Active<T>) {
        self.active.push(active);
    }

    pub(super) fn stage(&mut self, replacement: Active<T>) {
        debug_assert!(self.replacement_index().is_none());
        self.staged.push(replacement);
    }

    pub(super) fn is_replacement(&self, index: usize) -> bool {
        index >= self.active.len() && index < self.len()
    }

    pub(super) fn replacement_index(&self) -> Option<usize> {
        self.staged
            .iter()
            .position(|active| active.role != Role::Leaving)
            .map(|index| self.active.len() + index)
    }

    pub(super) fn activate_replacement(&mut self, index: usize) {
        if !self.is_replacement(index) {
            return;
        }
        {
            let replacement = self.staged.remove(index - self.active.len());
            self.fades[usize::from(replacement.slot.get())] = None;
            if let Some(victim) = self
                .active
                .iter_mut()
                .find(|active| active.slot == replacement.slot)
            {
                *victim = replacement;
            } else {
                self.active.push(replacement);
            }
        }
    }

    pub(super) fn remove(&mut self, index: usize) -> Active<T> {
        if index >= self.active.len() {
            return self.staged.remove(index - self.active.len());
        }
        let active = self.active.remove(index);
        self.fades[usize::from(active.slot.get())] = None;
        active
    }

    pub(super) fn fade_out(&mut self, slot: Slot, end: SessionFrame) {
        self.fades[usize::from(slot.get())] = Some(end);
    }

    pub(super) fn clear_fade(&mut self, slot: Slot) {
        self.fades[usize::from(slot.get())] = None;
    }

    fn fade_end(&self, slot: Slot) -> Option<SessionFrame> {
        self.fades.get(usize::from(slot.get())).copied().flatten()
    }

    pub(super) fn position(&self, find: impl Fn(&Active<T>) -> bool) -> Option<usize> {
        self.iter().position(find)
    }

    pub(super) fn get(&self, index: usize) -> Option<&Active<T>> {
        if index >= self.active.len() {
            self.staged.get(index - self.active.len())
        } else {
            self.active.get(index)
        }
    }

    pub(super) fn get_mut(&mut self, index: usize) -> Option<&mut Active<T>> {
        if index >= self.active.len() {
            self.staged.get_mut(index - self.active.len())
        } else {
            self.active.get_mut(index)
        }
    }

    pub(super) fn iter(&self) -> impl Iterator<Item = &Active<T>> {
        self.active.iter().chain(self.staged.iter())
    }

    pub(super) fn iter_mut(&mut self) -> impl Iterator<Item = &mut Active<T>> {
        self.active.iter_mut().chain(self.staged.iter_mut())
    }

    /// Indices of the tracks `find` picks, in slot-assignment order.
    pub(super) fn indices(&self, find: impl Fn(&Active<T>) -> bool) -> Vec<usize> {
        self.iter()
            .enumerate()
            .filter(|(_, active)| find(active))
            .map(|(index, _)| index)
            .collect()
    }

    pub(super) fn len(&self) -> usize {
        self.active.len() + self.staged.len()
    }

    delegate::delegate! {
        to self.parked {
            #[call(push)]
            pub(super) fn park(&mut self, parked: Parked<T>);
            #[call(len)]
            pub(super) fn parked_len(&self) -> usize;
            #[call(get_mut)]
            pub(super) fn parked_get_mut(&mut self, index: usize) -> Option<&mut Parked<T>>;
            #[call(remove)]
            pub(super) fn take_parked(&mut self, index: usize) -> Parked<T>;
            #[call(iter)]
            pub(super) fn parked_iter(&self) -> impl Iterator<Item = &Parked<T>>;
            #[call(iter_mut)]
            pub(super) fn parked_iter_mut(&mut self) -> impl Iterator<Item = &mut Parked<T>>;
        }
    }

    pub(super) fn parked_position(&self, find: impl Fn(&Parked<T>) -> bool) -> Option<usize> {
        self.parked.iter().position(find)
    }

    pub(super) fn tracks(&self) -> impl Iterator<Item = &T> {
        self.iter()
            .map(|active| &active.track)
            .chain(self.parked_iter().map(|parked| &parked.track))
    }

    pub(super) fn tracks_mut(&mut self) -> impl Iterator<Item = &mut T> {
        self.active
            .iter_mut()
            .chain(self.staged.iter_mut())
            .map(|active| &mut active.track)
            .chain(self.parked.iter_mut().map(|parked| &mut parked.track))
    }
}

/// `slot`'s last gain, silent when the mixer has not published it.
fn gain(deck: &DeckSnapshot, slot: Slot) -> f32 {
    deck.slots
        .get(usize::from(slot.get()))
        .map_or(0.0, |slot| slot.gain)
}
#[cfg(test)]
mod tests {
    use kithara_play::SlotSnapshot;
    use kithara_test_utils::kithara;

    use super::*;

    fn active(slot: u16, role: Role) -> Active<()> {
        Active {
            item: TrackId(u64::from(slot)),
            slot: Slot::new(slot),
            track: (),
            role,
            load: None,
        }
    }

    fn deck(gains: &[f32]) -> DeckSnapshot {
        DeckSnapshot {
            slots: gains
                .iter()
                .map(|&gain| SlotSnapshot {
                    gain,
                    ..SlotSnapshot::default()
                })
                .collect(),
            ..DeckSnapshot::default()
        }
    }

    #[kithara::test]
    fn the_lowest_slot_no_track_holds_is_free() {
        let mut slots = Slots::new(3);
        slots.push(active(0, Role::Current));
        slots.push(active(2, Role::Preloaded));

        assert_eq!(slots.free_slot(), Some(Slot::new(1)));
        slots.push(active(1, Role::Outgoing));
        assert_eq!(slots.free_slot(), None);
    }

    #[kithara::test]
    fn the_quietest_evictable_track_is_evicted() {
        let mut slots = Slots::new(3);
        slots.push(active(0, Role::Current));
        slots.push(active(1, Role::Outgoing));
        slots.push(active(2, Role::Leaving));

        let victim = slots.quietest(&deck(&[1.0, 0.25, 0.0]), |active| {
            active.role != Role::Leaving
        });

        assert_eq!(victim, Some(1));
    }
}
