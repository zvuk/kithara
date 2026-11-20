use kithara_command::Seq;
use kithara_play::{Outbox, PlayError, Position, Settled, Track, TrackStatus};
use kithara_render::bridge::SlotMark;
use kithara_signal::SessionFrame;
use kithara_sync::SpeedCurve;

use super::{
    Linked, LinkedPlayer,
    state::{Cause, ContinuationAdmission, SourceContinuation},
};
use crate::covers;

impl<P: Track> Linked<P> {
    /// Cues the incoming source with the outgoing track's accepted speed law,
    /// so both sources move alike through the entry window.
    pub(super) fn prepare_synced_continuation(
        &mut self,
        at: SessionFrame,
        position: Position,
        law: SpeedCurve,
        out: &mut Outbox<'_>,
    ) -> Result<bool, PlayError> {
        if !self
            .inner
            .prepare_source_continuation(at, position, law.clone(), out)?
        {
            return Ok(false);
        }
        if let Some(entry) = &self.continuation {
            return Ok(matches!(entry.admission, ContinuationAdmission::Preparing));
        }
        if !self
            .grid
            .as_ref()
            .is_some_and(|grid| covers(grid.geometry(), position))
        {
            return Ok(false);
        }
        if self.inner.cue(position, law, out)?.is_some() {
            self.continuation = Some(SourceContinuation {
                admission: ContinuationAdmission::Preparing,
            });
            self.cue = None;
            self.waiting = None;
            self.alignment = None;
            self.plan_end = None;
        }
        Ok(false)
    }

    pub(super) fn bind_continuation(&mut self, seq: Option<Seq>) {
        if let Some(entry) = &mut self.continuation
            && let ContinuationAdmission::Staged(to) = entry.admission
        {
            entry.admission = seq.map_or(ContinuationAdmission::Preparing, |seq| {
                ContinuationAdmission::Submitted { to, seq }
            });
        }
    }

    pub(super) fn settle_continuation(
        &mut self,
        entry: &SourceContinuation,
        settled: &Settled,
        cut: Option<(SlotMark, Position)>,
    ) {
        let ContinuationAdmission::Submitted { to, seq: submitted } = entry.admission else {
            return;
        };
        let (Settled::Applied { seq, at }, Some((mark, media_origin))) = (settled, cut) else {
            return;
        };
        let track = self.inner.snapshot();
        let track = track.as_ref();
        if *seq != submitted
            || !self.synced()
            || track.slot != Some(to)
            || !matches!(track.status, TrackStatus::Playing { since } if since == *at)
            || mark.session != *at
            || mark.position.checked_sub(media_origin).is_none()
            || track.mark.is_some_and(|current| {
                current.session >= mark.session && current.lane.segment != mark.lane.segment
            })
        {
            return;
        }
        self.alignment = Some(Cause::Drift);
    }
}
