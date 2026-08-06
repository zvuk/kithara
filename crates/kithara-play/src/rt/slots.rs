use super::track::PlayerTrack;

/// Position of a track in [`TrackSlots`].
///
/// Stable while the track lives: removing one track never shifts another, so a
/// slot collected at the top of a render pass still addresses the same track
/// further down it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct TrackSlot(usize);

/// The processor's fixed set of loaded tracks.
///
/// A track carries its own `src`, so lookup is a linear scan over at most
/// `CAPACITY` entries rather than a side table keyed by the same string.
/// Iteration order is slot order, which makes eviction and cleanup
/// deterministic.
pub(crate) struct TrackSlots<const CAPACITY: usize> {
    slots: [Option<PlayerTrack>; CAPACITY],
}

impl<const CAPACITY: usize> Default for TrackSlots<CAPACITY> {
    fn default() -> Self {
        Self {
            slots: [const { None }; CAPACITY],
        }
    }
}

impl<const CAPACITY: usize> TrackSlots<CAPACITY> {
    pub(crate) fn at_mut(&mut self, slot: TrackSlot) -> Option<&mut PlayerTrack> {
        self.slots.get_mut(slot.0)?.as_mut()
    }

    pub(crate) fn get(&self, src: &str) -> Option<&PlayerTrack> {
        self.iter()
            .find_map(|(_, track)| (&**track.src() == src).then_some(track))
    }

    pub(crate) fn get_mut(&mut self, src: &str) -> Option<&mut PlayerTrack> {
        self.iter_mut()
            .find_map(|(_, track)| (&**track.src() == src).then_some(track))
    }

    /// Whether every slot is taken. `insert` on a full set drops the newcomer,
    /// so callers evict first.
    pub(crate) fn is_full(&self) -> bool {
        self.slots.iter().all(Option::is_some)
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = (TrackSlot, &PlayerTrack)> {
        self.slots
            .iter()
            .enumerate()
            .filter_map(|(idx, slot)| Some((TrackSlot(idx), slot.as_ref()?)))
    }

    pub(crate) fn iter_mut(&mut self) -> impl Iterator<Item = (TrackSlot, &mut PlayerTrack)> {
        self.slots
            .iter_mut()
            .enumerate()
            .filter_map(|(idx, slot)| Some((TrackSlot(idx), slot.as_mut()?)))
    }

    pub(crate) fn len(&self) -> usize {
        self.slots.iter().flatten().count()
    }

    /// Place a track, replacing any track already loaded under the same `src`.
    ///
    /// Returns the replaced track, or the newcomer itself when the set is full
    /// — never silently drops it, since a `PlayerTrack` must not be freed on
    /// the audio thread.
    pub(crate) fn insert(&mut self, track: PlayerTrack) -> Option<PlayerTrack> {
        if let Some(slot) = self.slot_of(track.src()) {
            return self.slots[slot.0].replace(track);
        }
        match self.slots.iter_mut().find(|slot| slot.is_none()) {
            Some(slot) => {
                *slot = Some(track);
                None
            }
            None => Some(track),
        }
    }

    pub(crate) fn remove(&mut self, src: &str) -> Option<PlayerTrack> {
        let slot = self.slot_of(src)?;
        self.remove_at(slot)
    }

    pub(crate) fn remove_at(&mut self, slot: TrackSlot) -> Option<PlayerTrack> {
        self.slots.get_mut(slot.0)?.take()
    }

    fn slot_of(&self, src: &str) -> Option<TrackSlot> {
        self.iter()
            .find_map(|(slot, track)| (&**track.src() == src).then_some(slot))
    }
}
