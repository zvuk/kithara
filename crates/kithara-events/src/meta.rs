#![forbid(unsafe_code)]

use crate::{Event, SlotId, TrackId};

#[derive(Clone, Copy, Debug, Default)]
pub struct ScopeLabel {
    pub deck: Option<SlotId>,
    pub track: Option<TrackId>,
}

impl ScopeLabel {
    #[must_use]
    pub(crate) fn merged_with(self, child: Self) -> Self {
        Self {
            deck: child.deck.or(self.deck),
            track: child.track.or(self.track),
        }
    }
}

#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct EventMeta {
    pub origin: u64,
    pub seq: u64,
    pub ts_micros: u64,
    pub deck: Option<SlotId>,
    pub track: Option<TrackId>,
}

#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Envelope {
    pub meta: EventMeta,
    pub event: Event,
}
