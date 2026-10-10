use kithara_signal::SessionFrame;
use ringbuf::{HeapProd, traits::Producer};

use crate::bridge::{DeckEvent, RtMetrics, Slot};

/// What a track reports through for one render block: the events its owner reacts to, the
/// counters it samples, and where on the session clock the block starts.
pub struct RtSink<'a> {
    pub(super) events: &'a mut HeapProd<DeckEvent>,
    pub(super) metrics: &'a RtMetrics,
    /// The slot the track sits in.
    pub(super) slot: Slot,
    /// Session frame of the block's first frame.
    pub(super) start: SessionFrame,
}

impl<'a> RtSink<'a> {
    pub const fn new(
        events: &'a mut HeapProd<DeckEvent>,
        metrics: &'a RtMetrics,
        slot: Slot,
        start: SessionFrame,
    ) -> Self {
        Self {
            events,
            metrics,
            slot,
            start,
        }
    }

    /// Session frame `offset` frames into the block.
    #[must_use]
    pub fn at(&self, offset: usize) -> SessionFrame {
        let offset = i64::try_from(offset).unwrap_or(i64::MAX);
        SessionFrame::new(i64::from(self.start).saturating_add(offset))
    }

    /// Hand `event` to the owner; a full ring drops it here and counts it.
    pub(super) fn report(&mut self, event: DeckEvent) {
        if self.events.try_push(event).is_err() {
            self.metrics.record_event_overflow();
        }
    }
}
