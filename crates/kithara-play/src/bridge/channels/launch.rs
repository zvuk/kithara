//! Arm state and reanchoring of a slot's pending synchronized seeks.

use std::num::NonZeroUsize;

use kithara_events::TrackId;

use super::{ScheduledSeekReanchor, ScheduledTrackSeek, ScheduledTrackSeekState, SlotControl};
use crate::bridge::ScheduledSeekDisposition;

impl SlotControl {
    pub(crate) fn set_prepared_launch_armed(&mut self, item_id: TrackId, armed: bool) -> bool {
        for handoff in &mut self.prepared_launch_epochs {
            if handoff.item_id == item_id {
                handoff.armed = armed;
            }
        }
        let Some(seek) = self
            .scheduled_seeks
            .iter_mut()
            .find(|seek| seek.item_id == item_id)
        else {
            return false;
        };
        if !seek.disposition.is_prepared_launch() {
            return false;
        }
        seek.armed = armed;
        true
    }

    pub(crate) fn disarm_prepared_launches(&mut self) {
        for handoff in &mut self.prepared_launch_epochs {
            handoff.armed = false;
        }
        for seek in &mut self.scheduled_seeks {
            if seek.disposition.is_prepared_launch() {
                seek.armed = false;
            }
        }
    }

    /// Moves the track's pending seek onto a successor activation and warp
    /// map, keeping its kind and arm state. Returns whether it moved.
    ///
    /// A seek that has not begun moves in place. A begun seek has left for the
    /// activation source, so PCM of the successor revision needs a new decoder
    /// epoch: it begins again only while the successor activation lies beyond
    /// `lead` output frames of presentation, the budget a scheduled seek
    /// begins within; closer, the output moment stays with the callback.
    pub(crate) fn reanchor_scheduled_seek(
        &mut self,
        reanchor: ScheduledSeekReanchor,
        lead: NonZeroUsize,
    ) -> bool {
        let ScheduledSeekReanchor {
            item_id,
            position,
            expected,
            successor,
        } = reanchor;
        let queued = self
            .scheduled_seeks
            .iter()
            .position(|seek| seek.item_id == item_id);
        let pending = queued.map(|index| self.scheduled_seeks[index]);
        if pending.is_some_and(|seek| seek.disposition.activation() != expected) {
            return false;
        }
        let begun = pending
            .is_none_or(|seek| !matches!(seek.state, ScheduledTrackSeekState::AwaitingStart));
        let lead = i64::try_from(lead.get()).unwrap_or(i64::MAX);
        if begun
            && self
                .render_snapshot_for(item_id, None)
                .is_some_and(|snapshot| {
                    i64::from(successor.activation) - i64::from(snapshot.frontier().output())
                        <= lead
                })
        {
            return false;
        }
        let handoff = self
            .prepared_launch_epochs
            .iter()
            .find(|handoff| handoff.item_id == item_id)
            .copied();
        let (launch, armed) = pending.map_or_else(
            || handoff.map_or((false, false), |handoff| (true, handoff.armed)),
            |seek| (seek.disposition.is_prepared_launch(), seek.armed),
        );
        let disposition = if launch {
            ScheduledSeekDisposition::PreparedLaunch(successor)
        } else {
            ScheduledSeekDisposition::SeekOnly {
                activation: successor.activation,
            }
        };
        if let (Some(index), false) = (queued, begun) {
            self.scheduled_seeks[index].disposition = disposition;
            return true;
        }
        self.prepared_launch_epochs
            .retain(|handoff| handoff.item_id != item_id);
        self.scheduled_seeks.retain(|seek| seek.item_id != item_id);
        self.scheduled_seeks.push(ScheduledTrackSeek {
            item_id,
            position,
            disposition,
            armed,
            state: ScheduledTrackSeekState::AwaitingStart,
            observed_output: None,
        });
        true
    }
}
