use std::num::NonZeroUsize;

use kithara_command::Target;

use super::track::{PlayerResource, PlayerTrack};
use crate::bridge::Slot;

/// The tracks a mixer holds, one per slot its owner assigns.
pub(crate) struct TrackSlots {
    slots: Box<[Option<PlayerTrack>]>,
}

impl TrackSlots {
    /// `capacity` empty slots, allocated once: a mixer never grows them on the audio thread.
    pub(crate) fn new(capacity: NonZeroUsize) -> Self {
        Self {
            slots: (0..capacity.get()).map(|_| None).collect(),
        }
    }

    /// The track in `slot`; `None` for an empty slot or one past the mixer's count.
    pub(crate) fn at(&self, slot: Slot) -> Option<&PlayerTrack> {
        self.slots.get(slot.index())?.as_ref()
    }

    pub(crate) fn at_mut(&mut self, slot: Slot) -> Option<&mut PlayerTrack> {
        self.slots.get_mut(slot.index())?.as_mut()
    }

    /// Returns the resource if the slot is occupied or outside the deck.
    pub(crate) fn put(
        &mut self,
        slot: Slot,
        track: PlayerTrack,
    ) -> Result<(), Box<PlayerResource>> {
        let Some(entry) = self
            .slots
            .get_mut(slot.index())
            .filter(|entry| entry.is_none())
        else {
            return Err(track.into_resource());
        };
        *entry = Some(track);
        Ok(())
    }

    /// Take the track out of `slot`.
    pub(crate) fn take(&mut self, slot: Slot) -> Option<PlayerTrack> {
        self.slots.get_mut(slot.index())?.take()
    }

    pub(crate) fn replace(
        &mut self,
        slot: Slot,
        track: PlayerTrack,
    ) -> Result<PlayerTrack, Box<PlayerResource>> {
        let Some(current) = self.slots.get_mut(slot.index()).and_then(Option::as_mut) else {
            return Err(track.into_resource());
        };
        Ok(std::mem::replace(current, track))
    }

    pub(crate) fn iter_mut(&mut self) -> impl Iterator<Item = (Slot, &mut PlayerTrack)> {
        self.slots
            .iter_mut()
            .enumerate()
            .filter_map(|(index, held)| Some((slot(index), held.as_mut()?)))
    }

    /// Every slot, held or empty, in order.
    pub(crate) fn slots(&self) -> impl Iterator<Item = Slot> + use<> {
        (0..self.slots.len()).map(slot)
    }
}

/// The slot at `index`; a mixer never holds more slots than a `Slot` counts.
fn slot(index: usize) -> Slot {
    Slot::new(u16::try_from(index).unwrap_or(u16::MAX))
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;

    use kithara_platform::{sync::Arc, time::Duration};
    use kithara_signal::AudioSpec;
    use kithara_test_utils::kithara;

    use super::*;
    use crate::{
        rt::track::{PcmConsumer, PlayerResource},
        test_pools::pools,
        worker::packet_tests::PacketRing,
    };

    fn track(src: Arc<str>) -> PlayerTrack {
        let sample_rate = NonZeroU32::new(44_100).expect("static sample rate");
        let mut ring = PacketRing::new(AudioSpec::new(2, sample_rate), Duration::from_secs(1), 2);
        let resource = PlayerResource::new(
            PcmConsumer::new(ring.receiver.take().expect("receiver")),
            src,
            &pools(),
        )
        .map_or_else(|error| panic!("test player resource: {error}"), Box::new);

        PlayerTrack::builder()
            .sample_rate(sample_rate)
            .build(resource)
    }

    #[kithara::test]
    fn identical_sources_are_addressed_by_slot() {
        let src: Arc<str> = Arc::from("same.mp3");
        let (first, second) = (Slot::new(0), Slot::new(1));
        let mut tracks = TrackSlots::new(NonZeroUsize::new(2).expect("two slots"));

        assert!(tracks.put(first, track(Arc::clone(&src))).is_ok());
        assert!(tracks.put(second, track(src)).is_ok());
        assert!(tracks.at(first).is_some() && tracks.at(second).is_some());

        assert!(tracks.take(first).is_some());
        assert!(!tracks.at(first).is_some());
        assert!(tracks.at(second).is_some());
        assert!(tracks.at(Slot::new(2)).is_none(), "past the mixer's slots");
    }
}
