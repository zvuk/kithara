use kithara_signal::SegmentId;

use super::state::Position;

/// The lane segment a track currently follows.
///
/// A cue, seek, host-rate change or repeat issues a new segment and replaces
/// the whole value, so a new segment starts unready. A cancelled repeat
/// restores its parked segment the same way. `observe` fills a missing origin
/// from the first chunk the deck reports, and the lane's readiness report sets
/// `ready`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Segment {
    pub(super) id: SegmentId,
    /// The media position the segment starts from, once it is known.
    pub(super) origin: Option<Position>,
    /// The speed the segment starts at.
    pub(super) speed: f32,
    /// The lane's latest readiness report names this segment.
    pub(super) ready: bool,
}

impl Segment {
    pub(super) fn issued(id: SegmentId, origin: Option<Position>, speed: f32) -> Self {
        Self {
            id,
            origin,
            speed,
            ready: false,
        }
    }

    /// Records the lane's readiness report; a report for another segment
    /// leaves this one unready.
    pub(super) fn report_ready(&mut self, ready: SegmentId) {
        self.ready = ready == self.id;
    }
}
