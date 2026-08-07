use super::track::PlayerTrack;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct TrackSlot(usize);

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
    delegate::delegate! {
        to self.slots {
            #[expr($?.as_mut())]
            #[call(get_mut)]
            pub(crate) fn at_mut(&mut self, #[newtype] slot: TrackSlot) -> Option<&mut PlayerTrack>;
            #[expr($.all(Option::is_some))]
            #[call(iter)]
            pub(crate) fn is_full(&self) -> bool;
            #[expr($?.take())]
            #[call(get_mut)]
            pub(crate) fn remove_at(&mut self, #[newtype] slot: TrackSlot) -> Option<PlayerTrack>;
        }
    }

    pub(crate) fn get(&self, src: &str) -> Option<&PlayerTrack> {
        self.iter()
            .find_map(|(_, track)| (&**track.src() == src).then_some(track))
    }

    pub(crate) fn get_mut(&mut self, src: &str) -> Option<&mut PlayerTrack> {
        self.iter_mut()
            .find_map(|(_, track)| (&**track.src() == src).then_some(track))
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

    /// Place a track in a free slot, handing it back when every slot is taken — a `PlayerTrack`
    /// must not be freed on the audio thread.
    pub(crate) fn insert(&mut self, track: PlayerTrack) -> Option<PlayerTrack> {
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

    fn slot_of(&self, src: &str) -> Option<TrackSlot> {
        self.iter()
            .find_map(|(slot, track)| (&**track.src() == src).then_some(slot))
    }
}
