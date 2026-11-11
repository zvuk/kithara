use kithara_command::Seq;
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

/// One deck adoption of a segment, answered to `caller`.
pub(super) struct Adoption {
    pub(super) seq: Seq,
    pub(super) caller: Seq,
    pub(super) segment: SegmentId,
    /// The segment and position a repeat left, restored if it is cancelled.
    pub(super) parked: Option<(SegmentId, Position)>,
    pub(super) cancelled: bool,
}

/// The deck adoptions a track has open.
///
/// Segment issue and repeat push adoptions; their receipts take them back.
/// An adoption the deck refuses as stale or outdated leaves its caller in
/// `retry` until the deck has room to adopt the current segment again.
/// Release, a refused attach and returned PCM clear everything; a supersede
/// and a failed preparation drop only the retry.
#[derive(Default)]
pub(super) struct Adoptions {
    pub(super) inflight: Vec<Adoption>,
    pub(super) retry: Option<Seq>,
}

impl Adoptions {
    pub(super) fn take(&mut self, seq: Seq) -> Option<Adoption> {
        let index = self.inflight.iter().position(|adopt| adopt.seq == seq)?;
        Some(self.inflight.remove(index))
    }

    /// A deck adoption of `segment` is in flight.
    pub(super) fn adopts(&self, segment: SegmentId) -> bool {
        self.inflight.iter().any(|adopt| adopt.segment == segment)
    }

    pub(super) fn includes(&self, seq: Seq) -> bool {
        self.inflight.iter().any(|adopt| adopt.seq == seq)
    }

    delegate::delegate! {
        to self.inflight {
            /// A cancelled repeat is still in flight; lane commands wait for it.
            #[expr($.any(|adopt| adopt.cancelled))]
            #[call(iter)]
            pub(super) fn cancelling(&self) -> bool;
            #[expr($.any(|adopt| adopt.parked.is_some()))]
            #[call(iter)]
            pub(super) fn parking(&self) -> bool;
        }
    }

    pub(super) fn cancel_parked(&mut self) {
        for adoption in &mut self.inflight {
            if adoption.parked.is_some() {
                adoption.cancelled = true;
            }
        }
    }

    pub(super) fn clear(&mut self) {
        self.inflight.clear();
        self.retry = None;
    }
}
