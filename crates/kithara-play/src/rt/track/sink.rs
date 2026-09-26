use ringbuf::HeapProd;

use crate::bridge::{PlayerNotification, RtMetrics};

/// What a track reports through for one render block: discrete events the control thread reacts
/// to, counters it samples, and the slot seek epoch that block is rendering under.
#[derive(fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
pub struct RtSink<'a> {
    pub(super) notifications: &'a mut HeapProd<PlayerNotification>,
    #[field(get, vis = "pub(crate)")]
    pub(super) metrics: &'a RtMetrics,
    /// Latest seek epoch the control thread has published for this slot,
    /// sampled once per block. A track whose own epoch is older is rendering
    /// a position the user has already left.
    pub(super) seek_epoch: u64,
}

impl<'a> RtSink<'a> {
    pub const fn new(
        notifications: &'a mut HeapProd<PlayerNotification>,
        metrics: &'a RtMetrics,
        seek_epoch: u64,
    ) -> Self {
        Self {
            notifications,
            metrics,
            seek_epoch,
        }
    }

    pub(super) const fn reborrow(&mut self) -> RtSink<'_> {
        RtSink {
            notifications: self.notifications,
            metrics: self.metrics,
            seek_epoch: self.seek_epoch,
        }
    }
}
