use ringbuf::HeapProd;

use crate::bridge::{PlayerNotification, RtMetrics};

/// The two lock-free channels a track reports through: discrete events the
/// control thread reacts to, and counters it samples.
pub struct RtSink<'a> {
    pub(super) notifications: &'a mut HeapProd<PlayerNotification>,
    pub(super) metrics: &'a RtMetrics,
}

impl<'a> RtSink<'a> {
    pub fn new(
        notifications: &'a mut HeapProd<PlayerNotification>,
        metrics: &'a RtMetrics,
    ) -> Self {
        Self {
            notifications,
            metrics,
        }
    }

    pub(super) fn reborrow(&mut self) -> RtSink<'_> {
        RtSink {
            notifications: self.notifications,
            metrics: self.metrics,
        }
    }
}
